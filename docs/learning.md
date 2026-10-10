# Eve lessons

Eve tutors anyone, in any subject, at the level you state: a grade, a course, or
where you are ("I know Python, new to Rust"). The learning workspace implements the
two paths in the activity diagram: learn a topic, or bring a problem. It shares a saved lesson with the existing
voice agent and opens a mouse/pen whiteboard in a local browser or the macOS
widget helper window. The whiteboard is [tldraw](https://tldraw.dev); see
[The whiteboard](#the-whiteboard-tldraw).

## Try the working demo

From the repository root, with Python 3.11 or newer:

```powershell
python tools/start_learning.py --demo --data-dir .demo-data --port 48767
```

Open `http://127.0.0.1:48767/`. Click **Play workflow demo** to watch Eve
create an algebra problem, save an idea, give a hint, reveal three individual
steps, recap, and return to the saved dashboard. You can also use the controls
yourself. The demo has fixed addition, algebra, and derivative examples. It
never calls an API and explicitly does **not** recognize images or handwriting.
The offline demo is math-only; live mode takes any subject.
For a screenshot demo, choose **Help with my problem**, attach an image, type
`Solve 2x + 3 = 11.`, confirm it, and enter your ideas or select **I don't know
how to start**. Type `2x = 8` to see Eve continue with `x = 4`.

The recorded browser walkthrough is [eve-learning-demo.webm](learning-demo/eve-learning-demo.webm).
It exercises both paths against the real local server. The screenshots and
machine-readable check results are in [learning-demo/](learning-demo/).

## Live tutoring

The existing bridge daemon starts the learning server at
`http://127.0.0.1:48766/`. Restart the daemon after updating this checkout.
The browser window uses the same service as voice. Say **"lesson"** on its own: the
lesson window opens at once, with no model in the loop, and eve asks "Learn a topic,
or bring a problem?". Answer in your own words ("learn a topic, fractions, grade four");
until a lesson starts, every answer goes to the reasoning backend, which starts it.
The word is a second phrase in the same wake-word spotter, with its own stricter
threshold (bench, 2026-09-15: at the wake word's 0.25 the word "listen" fired on one of
three synthesized voices, at 0.35 never). `EVE_LESSON_WORD` changes the word,
`EVE_LESSON_WORD=off` removes it. Any sentence with "lesson" in it opens one while
eve is idle; inside a conversation the spotter is muted. Or say
**"okay eve, teach me something"** or **"okay eve, I want to do a math lesson"**
(the default wake-word file also includes "hey eve"
and "ok eve"). Eve asks which path you want, and asks for a topic and
starting level when needed. It uses the `lesson` tool instead of taking
over the computer with the computer-control agent.
If the voice answers a "teach me" or "quiz me" itself instead of opening the lesson, the
session notices (the phrase table in `intent.py`, then the intent classifier) and hands the
words to the backend as a `[lesson request]`, which opens the lesson.

Alternatively, run the learning service by itself:

```powershell
python tools/start_learning.py
```

Or, with the bridge installed: `eve-bridge learning`.
Do not run two learning servers on the same port. Standalone mode provides
the whiteboard and tutoring; robot voice requires the existing daemon, its
microphone/wake-word setup, and a working voice API configuration.

## Drive a lesson from the terminal

`eve-bridge lesson <action>` sends one lesson action to the running daemon.
It uses the same code path as the `lesson` voice tool, so the whiteboard,
the saved lesson, and the robot stay in sync. The actions are `open`, `start`,
`ideas`, `hint`, `check`, `step`, `status`, `recap`, `end`, `listen`, and
`stop-listening` (see [Think out loud](#think-out-loud)).

```bash
eve-bridge lesson open
eve-bridge lesson start --mode learn --topic "Fractions" --level "Grade 4"
eve-bridge lesson start --mode help
eve-bridge lesson ideas --text "I think I add the tops"
eve-bridge lesson hint
eve-bridge lesson check
eve-bridge lesson step
eve-bridge lesson status
eve-bridge lesson recap
eve-bridge lesson end
```

| Option | Use with | Meaning |
| --- | --- | --- |
| `--mode learn` / `--mode help` | `start` | Learn a topic, or get help with your own problem |
| `--topic`, `--level` | `start` | The topic and starting level for a new lesson. Level is free text: `"Grade 4"`, `"first-year physics"`, `"I know Python, new to Rust"` |
| `--text` | `ideas` | Your thinking; an empty text means "I don't know how to start" |
| `--socket` | any | IPC path or host:port override |

What happens:

- The command prints eve's answer. `status` also prints the open lesson's
  topic, mode, stage, and problem.
- The robot shows **thinking** while the tutor works (`start`, `hint`, `check`,
  `step`, `recap`), then one caption with the answer. When a voice conversation
  is open, the voice owns the screen and the robot shows nothing extra.
- A lesson action ends a manual explore first. `status` does not.
- Tutor replies can take up to about 2 minutes. The command waits that long.
- Exit code 0 means success. Exit code 1 means eve refused, for example when
  no lesson is open; the reason goes to stderr. Exit code 2 means the daemon is
  not reachable: start it with `eve-bridge daemon` or the launchd service.
  A tutor reply that takes longer than the wait also shows as exit code 2; run
  `eve-bridge lesson status` to see the result.
- With `EVE_LEARNING=0`, or when the learning server did not start, every
  action fails with a message that the workspace is unavailable.

`eve-bridge learning` is a different command. It starts the whiteboard
server and opens the browser without the daemon or the robot.

## Think out loud

A learner can talk through a problem, or ask questions in their own words.
eve listens and gives short feedback on the robot's screen.

### Start and stop listening

A lesson must be open, with its problem ready (stage "In progress" or
"Completed"). If not, eve says what to do first.

| Way | Start | Stop |
| --- | --- | --- |
| Voice, in an eve conversation | "Can I think out loud?", "Let me talk it through" | "I'm done", "stop listening", "bye" |
| Web app, lesson page | **◉ Think out loud** | **■ Stop listening** (the same button) |
| Terminal | `eve-bridge lesson listen` | `eve-bridge lesson stop-listening` |

All three use the same `listen` and `stop-listening` lesson actions. Pressing
start twice, or starting by voice while the button is on, does not open a
second session.

Listening also stops:

- after about 60 seconds with nothing said,
- at the conversation cap (10 minutes, the same cap as every conversation),
- when a touch on the robot hushes it.

The toggle needs the robot app (the daemon). `eve-bridge learning` runs
the web app by itself. There, the button is off and the page says that the
robot app is not running.

### What eve does

- No wake word is needed. Starting opens a normal eve conversation that
  waits through thinking pauses.
- If a "hey eve" conversation is already open, it switches to listening.
- eve mostly listens. At a pause it says one or two short sentences: some
  encouragement, a guiding question, or where the first slip is.
- Questions get hints. eve never says the final answer and never does a step,
  unless the learner asks to see one step. That step goes through the lesson,
  so it shows on the whiteboard.
- Each thing the learner says is added to the end of their ideas. Typed ideas
  are never replaced. If the learner types while eve saves a line, the save
  keeps both.
- While listening, eve does not run computer tasks, explore, or remember the
  conversation in its chat memory.
- The robot shows the listening pose with solid blue lights while it listens. It
  drops the pose to act out thinking and speaking, then shows it again.
- The web app shows a **eve is listening** badge in the page header and the
  lesson panel for the whole time. It checks the state every 1.5 seconds.

The listening rules and the lesson (problem, stage, ideas, eve's steps) are
in `bridge/src/eve_bridge/learning/think_aloud.py`. A session opened for
listening gets them in its start instructions. A conversation that switches
gets them as silent context, and the backend gets them through
`session.update`, because the voice instructions cannot change after start.

### Privacy

- The microphone audio goes to the voice session only while listening (or a
  "hey eve" conversation) is on. Stopping closes the session.
- No audio is saved.
- The learner's words are saved only into their lesson's ideas, on this
  computer. They are not written to logs or to eve's chat memory. The
  lesson events eve publishes on its memory bus (docs/memory-bus.md) carry
  the action, stage, topic, level and eve's own feedback, never the
  learner's words, strokes or images.
- Logs record the action and whether it worked (for example
  `lesson: listen -> ok=True`, `voice: spoken idea saved ok=True`).
- Separately, the "hey eve" wake-word detector runs on this computer all the
  time while voice is on (`EVE_VOICE=0` turns it off, and think out loud
  with it). It sends nothing anywhere until it hears its name.

### Tutor provider: OpenRouter

The tutor uses **OpenRouter** with `OPENROUTER_API_KEY` and the model
`openai/gpt-6-astra`, like every other model call in eve
([one key: OpenRouter](openrouter.md)). Until 2026-10-08 the default was OpenAI
with `OPENAI_API_KEY`; that path is gone, and a leftover
`EVE_LEARNING_PROVIDER=openai` now also means OpenRouter. Put the key in
`~/.config/eve-bridge/env` (on Windows:
`%USERPROFILE%\.config\eve-bridge\env`). The file is outside the repository:

```dotenv
OPENROUTER_API_KEY=
```

Restart the learning server or daemon after saving the key. Start **without**
`--demo`: `python tools/start_learning.py`. The dashboard displays the provider
and model, and reports a missing OpenRouter key without exposing its value.
All questions, screenshots, handwritten work, hints and steps use Astra; there
is no automatic switch to a cheaper model. OpenRouter can select an upstream
provider serving that same model.

The adapter uses OpenRouter's Chat Completions endpoint with base64 images and
strict JSON schema, requiring endpoint support for the requested parameters.
[Astra's OpenRouter page](https://openrouter.ai/openai/gpt-6-astra) lists its model
ID and image/structured-output support; see also
[OpenRouter structured outputs](https://openrouter.ai/docs/guides/features/structured-outputs).
The key stays on the Python server. OpenRouter and its selected provider handle
submitted lesson data according to their account/data policies.

Without `OPENROUTER_API_KEY` the tutor asks for it. An `OPENAI_API_KEY` is never
used instead. Eve's separate realtime voice connection (the desk voice) still
needs its OpenAI key until it moves to OpenRouter. Browser Read aloud remains
available.

| Setting | Meaning |
| --- | --- |
| `EVE_LEARNING=0` | Disable the daemon's learning server |
| `EVE_LESSON_WORD` | The spoken word that opens a lesson, default `lesson`; `off` removes it |
| `EVE_LESSON_THRESHOLD` | Spotter threshold for that word alone, default `0.35` (the wake word keeps `0.25`) |
| `EVE_LEARNING_PORT` | Daemon/widget port, default 48766 |
| `EVE_LEARNING_DIR` | Local store, default `~/.config/eve-bridge/learning` |
| `EVE_LEARNING_PROVIDER` | `openrouter` (the only provider; a leftover `openai` means the same) |
| `EVE_LEARNING_MODEL` | `openai/gpt-6-astra` (default); a bare OpenAI name gets its `openai/` prefix |
| `OPENROUTER_API_KEY` | Tutor key |
| `TINYFISH_API_KEY` | Optional TinyFish key for practice references (the same key the watcher and web lookups use) |
| `EVE_TLDRAW_LICENSE_KEY` | Optional tldraw licence key, passed to the whiteboard. See [Licence](#licence) |
| `--demo` | Explicit offline examples; separate default `learning/demo` store |
| `--data-dir` / `--port` | Standalone and demo overrides |

## Activity workflow

1. **Learn a topic:** choose topic and starting level, free text: a grade, a
   course, or where you are. Eve generates a problem without revealing its answer.
2. **Help with my problem:** paste/upload a PNG, JPEG, or WebP, write on the
   board, or type a problem. Eve transcribes the problem. Correct unclear
   symbols and explicitly confirm before tutoring begins.
3. Put down ideas. The help path asks for a starting attempt or an explicit
   "I don't know how to start" before it gives help. Writing the original
   problem does not count as a new attempt.
4. Drawing is optional. The board is a tldraw canvas: pen, eraser, text, shapes,
   arrows, select and move, undo and redo, zoom and pan, all from tldraw's own
   toolbar. The tutor sees a flattened picture of the whole board. The board
   locks while eve is answering and when a lesson is ended, then gives the
   learner's tool back.
   **Hint** nudges without completing a step. **Check my work** reviews current
   writing and ideas. **Show one step** adds one next transformation to Eve's
   separate panel. Click again for the next step. The learner's board is never
   overwritten by the tutor.
5. Optional supervision checks after a five-second pause following an edit.
   It does not continuously poll the model or interrupt individual pen strokes.
6. When complete, ask for a recap/understanding check, try another problem,
   change topic, or end. A lesson can be ended at any stage and resumed later.

The voice tool supports open, start, ideas, hint, check, step, status, recap,
end, listen, and stop-listening. Selecting a lesson in the browser makes it the voice tool's active
lesson. The browser polls saved revisions to display voice-originated updates.
The optional browser **Read aloud** control uses the operating system/browser
speech synthesizer; it is labelled separately from the robot's existing voice.

## The whiteboard (tldraw)

The board is [tldraw](https://tldraw.dev) 5.4.2, started from the basic starter
kit (`npm create tldraw@latest`, the `tldraw/vite-template` repository). The rest
of the page is still plain JavaScript with no build step. tldraw is an island:

| Piece | Where |
| --- | --- |
| Source (React, TypeScript, Vite) | `bridge/web-canvas/` |
| The seam between page and board | `window.EveCanvas.mount()` in `bridge/web-canvas/src/main.tsx`, used by `placeBoard()` in `learning/web/app.js` |
| Built output, committed | `bridge/src/eve_bridge/learning/web/canvas/` (`canvas.js`, `canvas.css`, `assets/`, `TLDRAW-LICENSE.md`) |
| Served by | the `/canvas/` route in `learning/server.py` |

The built output is committed so that `python tools/start_learning.py --demo`
still works from a fresh clone without Node. Rebuild after changing anything in
`bridge/web-canvas/`, or after moving the tldraw version:

```bash
cd bridge/web-canvas
npm install
npm run build      # typecheck, bundle, copy tldraw's fonts/icons/translations and licence
node scripts/verify-bundle.mjs
npm run e2e        # real Chrome, real server, real CSP; needs Google Chrome
```

If `web/canvas/` is missing, the lesson page says so where the board would be,
and the ideas box and the tutor keep working.

### What is saved

A save carries `board`: tldraw's document snapshot (`getSnapshot(store).document`),
and `board_image`: a PNG of everything on the board, at most 1600 px on a side,
which is what the tutor reads. Both are sent only when the learner changed the
board. The server checks the envelope (`store` and `schema`, records are
objects) and the size (4 MB); tldraw validates and migrates its own records when
it loads them. A pasted image lives inside the document as a data URL, capped
at 2 MB each.

"How much has the learner put on the board" (`mark_count` in `server.py`) is
the number of tldraw shapes plus any strokes from before tldraw. The help path
uses it to tell a new attempt from the problem that was already written down.

### Lessons saved before tldraw

Older lessons have `strokes` (pen, eraser, text boxes) and no `board`. Opening
one places its saved picture on the tldraw board as a locked image, exactly as
the learner left it, erased parts included; new work goes on top. Nothing is
saved until the learner changes something, and the old `strokes` are kept.
Eraser strokes cannot be turned into tldraw shapes faithfully, which is why the
picture is used rather than a conversion.

### Content policy

The page stays same-origin only. tldraw needs two allowances, both in
`content_policy()` in `server.py`:

- `img-src blob:`. tldraw shows and exports pictures through `blob:` URLs. Only
  a script already running in the page can create one.
- A nonce in `style-src`, new for every page load. tldraw adds `<style>`
  elements at run time (its licence watermark, and the fonts it embeds in a
  board picture) and stamps them with the nonce. There is no `'unsafe-inline'`:
  the page puts tutor and search text into `innerHTML`, and the policy stands
  behind that escaping.

tldraw's fonts, icons and translations are served from `/canvas/assets/`, never
from tldraw's CDN. The e2e test fails on any CSP violation, any request that
leaves the computer, and any console error.

### Licence

tldraw is not open source. Its [licence](../bridge/web-canvas/TLDRAW-LICENSE.md)
allows development use without a key, asks for a licence key for production
use, and forbids disabling or interfering with its licence enforcement, which
includes the "made with tldraw" watermark. A verbatim copy of the licence ships
next to the bundle, as the licence requires.

The lesson board hides the SDK watermark and licence prompt with a scoped rule
in `bridge/web-canvas/src/index.css`. The e2e test verifies this on desktop,
mobile, and after reload; it also removes the rule temporarily to confirm that
the underlying watermark is present and otherwise visible. This presentation
change does not grant a licence or change the upstream terms. The licence text
still ships with the bundle, and `EVE_TLDRAW_LICENSE_KEY` in
`~/.config/eve-bridge/env` is still passed through to tldraw.

`create-tldraw@5.4.2` itself crashes on start ("it's bad": it calls `main()`
before its telemetry URL list is assigned), and its `--no-telemetry` flag is
parsed as `telemetry=false`, so it does not opt out. The template was cloned
from `tldraw/vite-template` instead, which is all the CLI does.

## Saved work and dashboard

SQLite transactions save the problem, source screenshot, typed ideas, the
whiteboard (its tldraw document), flattened whiteboard image, tutor hints/steps/feedback, mode,
level, and completion state. Every autosave and tutoring action has a revision.
**Saved work** opens those revisions; **Export** downloads the lesson and its
full revision history as JSON. Ending a lesson preserves its previous stage.
Revision checks reject stale writes from another window instead of overwriting
newer work. The browser keeps a temporary local recovery draft while edits are
unsaved, and warns before closing with unsaved work.

The files are `lessons.sqlite3` and a small `dashboard.json` widget summary.
They are separate from the room diary, automatic memories, and photo pruning.
All saved revisions are retained, including erased writing; history can grow
large over time. Protect/back up the directory as appropriate for learner data.
Uploaded images are resized to a maximum dimension of 1600 pixels. Live checks
send the selected screenshot, current board, and relevant lesson context to
the tutor. No automatic desktop capture is used.

## Widget integration (macOS)

Build the existing `widget/StackChanNotes.xcodeproj` in Xcode on a Mac. All Swift
changes are in files already included in that project; regeneration is not
required. Start the helper and bridge daemon, then add **eve's learning**
from the widget gallery. The helper mirrors the summary into its App Group
every 30 seconds. The card shows lesson counts and recent problems; click it
to open the full dashboard embedded in the helper app. The existing diary
widget's medium/large layouts also contain a **Learning dashboard** link,
and the menu-bar app has an **Open learning dashboard** command. The older
Python `notes-widget` panel also includes this command in its right-click menu.

If using a custom directory or port, give the helper app the same environment
values as the daemon. The demo is separate from live widget data unless you
explicitly point the helper at the demo directory and port. Windows uses the
browser dashboard; WidgetKit is only available on macOS.

## Verification and current limits

`bridge/tests/test_learning.py` covers the flow, exactly-one-step demo behavior,
the tldraw board (validation, mark counting, the `/canvas/` route, the content
policy and its nonce, the licence key setting),
current image inputs, API response validation, durable revisions, conflicts,
failure recovery, lesson lifecycle, voice delegation, and local HTTP request
boundaries. `bridge/tests/test_learning_search.py` covers TinyFish search and env-file
loading. `bridge/tests/test_daemon_lesson.py` covers `eve-bridge lesson` and
the daemon's IPC handler. Voice tests cover the tool through the existing connection.
`bridge/tests/test_learning_think_aloud.py` covers think out loud on the learning
side (actions, the browser toggle, the standalone answer, spoken ideas that
append, the listening instructions). The think-out-loud tests at the end of
`test_voice_agent.py` and `test_daemon_lesson.py` cover the session (silence
stop, cap, saved and unlogged words) and the daemon (IPC, races, robot pose).

```powershell
python -m pytest bridge/tests/test_learning.py bridge/tests/test_learning_search.py bridge/tests/test_voice_agent.py bridge/tests/test_daemon_lesson.py -q
# With Playwright installed (plus its ffmpeg component for video recording):
python tools/demo_learning.py
# The whiteboard itself, in real Chrome under the real CSP:
cd bridge/web-canvas && npm run e2e
```

The demo runner needs `bridge/src` on `PYTHONPATH` (or an editable bridge install)
and Chrome at its standard Windows path; `--browser` accepts another executable.
It runs headless and saves a video and screenshots. Playwright's test context
bypasses CSP only for its assertion machinery; the application keeps its strict
same-origin content policy. The live tutor is model-based, not an independently
verified engine for any subject. The single-step schema and instruction
constrain its output, but they cannot prove that every generated step is
correct or pedagogically atomic. Handwriting accuracy and level-appropriate
teaching quality need real learner evaluations before classroom use.

The recorded demo (Windows, 2026-09-12) uses offline examples.
Live image/voice API calls, physical robot behavior, and the Swift widget build
require their respective API access/hardware/macOS environment and are not
represented as verified by that demo.


### Verification in this checkout (Windows, 2026-09-12)

- Targeted learning/voice/wake-word/daemon/widget suite: **142 passed, 3 skipped**.
- Browser demo: **15 workflow checks passed**, no JavaScript page errors.
- Changed Python files: Ruff passed.
- Full bridge suite with dependencies installed and UTF-8 enabled: **993 passed,
  9 failed, 5 skipped**. Remaining failures are in chat-memory/identity/notes/
  photos/recall POSIX-permission assertions, environment home expansion,
  read-policy POSIX paths, and a timing-sensitive state ordering assertion.
  These modules were not modified for learning. The full suite is not green.
- Live API key: absent; live image/voice calls not exercised.
- macOS Swift build and physical robot: not available in that Windows run.


### Running from WSL with a Windows environment file

WSL's `~` is the Linux home, not the Windows home. To use the same key file:

```bash
python tools/start_learning.py --env-file /mnt/c/Users/<you>/.config/eve-bridge/env
```

Alternatively set `EVE_ENV_FILE` to that path before launching the bridge
or learning server. Explicit `--env-file` takes precedence over that variable;
existing shell settings still take precedence over values inside the file.
Restart the server after changing settings. Windows PowerShell users can keep
using `python tools/start_learning.py` with the default Windows home path.


### OpenRouter 403: provider Terms of Service restriction

If Eve reports this specific restriction, the request reached OpenRouter but
was rejected under a provider policy. A simple addition-generation request also
returned this error during diagnosis on 2026-09-12. The response does not name
the specific policy or identify whether the restriction is account-wide or
model-specific. Ask OpenRouter support to review access to `openai/gpt-6-astra`.
Changing the worksheet or adding an OpenAI key is not an established fix for
this error. Eve preserves saved work and does not automatically retry or
switch models. Raw response metadata and account identifiers are not shown.


### Address already in use

Only one service can listen on port 48766. If Eve is already running, open
`http://127.0.0.1:48766/` instead of launching it again. To reload changed
settings, stop that specific Eve process, then restart it with the same
arguments. In WSL, `pgrep -af start_learning.py` identifies standalone launchers.
Do not terminate WSL itself or unrelated processes. The launcher now reports
an occupied port with this guidance and exits with status 2 instead of showing
a traceback. `--port 48768` can launch a separate instance when intended.


### TinyFish practice references

Add `TINYFISH_API_KEY=your-tinyfish-key` to `~/.config/eve-bridge/env`, the same
bridge environment file, and restart the service. The daemon,
`eve-bridge learning`, `python tools/start_learning.py`, and
`python -m eve_bridge.learning` all read that file at startup. Variables
already set in the environment win over values in the file. No additional Python packages are required. Live lesson generation
searches TinyFish once for the topic and level, then asks the configured tutor to
create one original adapted problem from relevant references. Help with an
existing problem, checks, hints, and steps do not trigger searches. Demo mode
makes no search requests.

The integration uses [TinyFish Search](https://docs.tinyfish.ai/search-api), then
[TinyFish Fetch](https://docs.tinyfish.ai/fetch-api) to read the top three pages as
markdown. Both are free. There is no site restriction, so any subject can find
references. Only topic and level are sent to TinyFish; learner work and images are
not sent.

Until 2026-09-26 this used Exa with its content moderation on, then Firecrawl
until 2026-09-29. **TinyFish's search has no moderation option**, so for young learners the filter is now the
tutor alone: it writes an original, adapted problem and never shows the pages'
text. The reference titles and links are still shown in the lesson feedback.
Retrieved text is treated as untrusted reference material. Search cannot
independently verify correctness or guarantee an appropriate result.

The lesson feedback shows the search outcome and expandable reference links
(which may lead to pages containing answers). Links are saved in lesson events
and remain available when reopening a lesson from the widget/dashboard. These
are search references, not a claim that a particular exercise was copied from
a page. Raw retrieved text is not saved. Missing keys, empty results, and search
errors fall back to ordinary problem generation with a visible explanation.
The tutor API key is still required; TinyFish uses its own account.
