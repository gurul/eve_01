# Texting eve in Eve Link: the app's one screen

The owner is moving from Telegram to texting eve in Eve Link, the iPhone
app. On 2026-10-08 the owner asked for it to be "very simple minimal very very
clean", like the ChatGPT app, and "I just don't want to interact with a webpage
on my app". So the app is one native screen with no tab bar. Only eve's
messages are a web view.

## The screen

Pure black in dark mode (the system background in light). Nothing is on the
screen but the controls below until there are messages.

- **Top bar.** At left, a round 44 pt button with a two-line menu glyph opens
  **Settings**. A small blue dot is on it when a job waits on the owner (a Go,
  an answer or a budget raise). In the center, a capsule pill **Chat | Apps |
  Work** (owner, 2026-10-08: "chat, apps, work"); the selected segment is a
  lighter filled capsule. A tap swaps the content at once and only the
  highlight slides. The chat's web view stays alive, hidden, on Apps and Work:
  rebuilding it on every switch reloaded the page and made switching jittery. At right, a round 44 pt button
  opens **voice mode**. Its waveform is red and moves while a call is open.
  While the chat shows, the bar sits on a fade: the screen's color behind the
  status bar, fading to clear at the bar's bottom edge. A message scrolled up
  fades out under the bar and never meets the clock.
- **Chat** (`App/ChatScreen.swift`). eve's messages fill the screen. At the
  bottom is one rounded card (24 pt corners, a 1 px edge, dark gray): the field
  ("Ask eve", grows to six lines) on top, and a row below it with **+** at
  left and the speed dial, the mic and the send button at right. Send is a
  white circle with a black up arrow; it turns into a stop square while eve
  answers, and into a small spinner while attachments upload.
  - **+** (`App/ChatAttach.swift`) is a menu with **Photos** (the system photo
    picker), **Files** (the Files app) and **Commands** (the / menu, below).
    What you pick shows above the field in the card: a photo as a 56 pt rounded
    thumbnail, a file as a small card with its name, each with an x to take it
    off. Four at most; then Photos and Files are dimmed. Send works with only
    attachments and no words. See "Attachments".
  - **The / menu** (`App/ChatSlash.swift`, `Shared/SlashCommands.swift`; owner,
    2026-10-08: "have menu feature in chat"): Telegram's command menu, in the
    app. While the field starts with "/" and has no space yet, a small card in
    the composer's style (20 pt corners, the same fill and edge) sits just above
    the composer card. Each row is "/command" and its one-line description,
    truncated. The rows filter by prefix as you type ("/c" shows /claude_on,
    /claude_off and /codex), at most six show and the rest scroll. A tap sends
    "/command" at once, the way the send button sends (Telegram's menu sends on
    tap too), clears the field, and gives a selection haptic. A space, a line
    break, deleting the "/", or no match hides it. **Commands** in the + menu
    types "/" and opens it. It does not show while attachments wait: a code word
    goes alone. VoiceOver reads each row as "claude on command", its
    description, and "Sends /claude_on to eve".
    The words are the daemon's (`GET /api/link/commands`, the same list
    Telegram's menu gets, so /jobs is there only while the chief is on), read
    when the chat first shows and again for a new pairing, and kept in memory.
    Until that read lands, or when it fails (an older eve without the list),
    the menu shows a built-in copy of the 14 base words, so it is never empty;
    `tests/test_link_commands.py` holds that copy to `telegram.BOT_COMMANDS`.
    The daemon handles a sent word exactly as one typed.
  - **Speed dial** (`gauge.with.dots.needle.33percent`): a menu with **Auto**
    (the default), **Instant** and **Think**. The choice is remembered
    (`UserDefaults` `chat.grade`) and goes with every turn as `"grade"`. When it
    is not Auto, its name shows blue next to the gauge.
  - **Mic** is dictation, not a call: the system speech recognizer
    (`App/Dictation.swift`) types what you say into the field, after what was
    already typed. Tap again to stop. Nothing is sent until you tap send.
    Recognition is on the device when the phone can do it. Dictation does not
    start while a call is open. Opening voice mode or starting a call (the Action
    Button too) stops it first, and leaves the call's audio session alone. It
    also stops when the app goes to the background.
- **Apps** and **Work** (`App/WorkView.swift`, one plain list each). **Apps**
  is eve's controls (Music, Lights, Remote); **Work** is the chief's jobs.
  Pull down to refresh; each also refreshes when its tab opens, and both are
  read at launch, so a tab opens on data, not "Loading…".
  - A job shows its title, one muted status line ("Waiting on your Go",
    "Researching", "Done": the daemon's `line`) and a thin line of the steps
    done. Open jobs come first. A blue dot marks one that waits on the owner.
    A tap opens a sheet with the end state, the steps, spent and budget, and
    only the actions that apply now: **Go**, **Done**, **Raise budget**,
    **Drop** (Drop asks first). A Go sends the revision the sheet showed, so a
    job changed since is refused and the list refreshes. A job whose act may
    already have happened (after a restart, or a step stopped at its ceiling)
    gets **It happened** and **It didn't**, never a Go: a tap must not run the
    act twice. After "It didn't", the Go comes back.
  - **New job** (owner, 2026-10-09: "work tab should allow me to start long
    horizon task not only just track"). Work has its own bar over the list:
    "Jobs" and a round **+**. It opens a **New job** sheet: a multiline goal
    ("Plan my trip to Rome in December, flights and a hotel under $200 a
    night"), optional details, Cancel and **Start**. eve hears it as a chat
    turn ("Take this on as a job: …") and files the card through `take_on`, as
    it would in the chat, with the same rules: a step it cannot undo still
    waits on your Go. On success the sheet closes, the list refreshes and the
    new job's sheet opens: the job this request's own turn filed, never one
    another chat turn filed at the same moment. eve's "On it: …" shows in
    the chat, ending `Reply "change c12" or "drop c12".`; typing either back
    does what Telegram's Change and Drop buttons do. When eve files no card (it asks a question back,
    or answers another way), the sheet shows eve's words, or why, in red;
    the question waits in the chat. The same words sent again (a dropped
    connection, a slow turn) wait for the first try, never a second job;
    changed words are a new job.
  - An error says which part is off: "eve's chief is off." for Jobs.
  - **Apps** are eve's own controls (owner, 2026-10-08: "like the spotify,
    lights and remote"): **Notes** (2026-10-10, below), then **Music**, **Lights** and **Remote**, the Telegram
    Mini App's three cards, each listed only when this Mac has it. **Memory** (2026-10-09, owner: "make memories
    accessible through app") is in **Settings** since 2026-10-10 (owner: "move memories to settings"), not here. A row is an
    SF Symbol in a rounded square, the name, and one muted line ("Spotify",
    "3 lights", "Fire TV"). A tap opens a native sheet (`App/ControlSheets.swift`)
    with the same actions as the Mini App card, and the Mac's line under the
    controls (VoiceOver reads it out):
    - **Notes** (`App/NotesView.swift`, `Shared/NotesText.swift`, `link_notes.py`; owner, 2026-10-10: "an app
      where I can look at my notes and interact with them change delete updates", "essentially like a mini
      notion", "make sure it's mobile native"): the second brain's vault (docs/stackchan/second-brain.md), listed
      while `EVE_SECOND_BRAIN` is on and the vault folder is there. The list is the vault's folders as
      collapsible sections (Inbox, To-dos, Projects · Garden, …; Archive starts closed), each note's title, date
      and first line, newest first. The search field filters titles as you type; **Search** looks inside every
      note. Swipe a note to **Delete**; a long press offers **Move to** a folder. A delete goes into the vault's
      hidden `.eve-trash/` and the bar at the bottom offers **Undo** for six seconds; nothing is ever unlinked.
      The pencil (top left) starts a page: a title, a folder and markdown. A page draws its markdown natively:
      headings, to-dos with a box you tap to tick (saved at once), lists, quotes, code and rules, `[[wiki
      links]]` as their words; the frontmatter is a muted line on top. **Edit** turns the body into a text
      editor; **Done** saves. The ••• menu has Move to, **Undo last change** (the save's history,
      `second_brain.undo_note`) and Delete. Every save sends the revision the phone read: when the file changed
      on the Mac since (Obsidian, or eve), nothing is written and the page says "This note changed on the Mac"
      with a button to open the Mac's version. The frontmatter is never edited from the phone; a note longer
      than 40,000 characters is read-only here.
    - **Music**: the artwork (Spotify's image host only), track, artists and
      album; previous, play/pause, next; a volume slider (sent when you let
      go); the device it plays on, as a menu that moves the music; and the
      Voice PE's Spotify mode as a switch, when the board is eve's
      controller.
    - **Lights**: which lights (All, each room, each light: shown only with
      more than one light), On and Off, the Mini App's ten colour swatches and
      the system colour picker (while you drag, one colour is on its way and
      only the newest waits), and a brightness slider. Opening it reads every
      light ("desk lamp: cyan 60% · strip: off").
    - **Remote**: what the TV says now ("TV on — YouTube, volume 44%") and a
      power button; the TV's apps as chips; a round d-pad with OK; Back, Home,
      Menu; rewind, play/pause, fast-forward; volume down, mute, volume up
      (10% a press); and a volume slider. Every target is 44 pt or more and
      every press taps the haptic engine. Presses go in the order they were
      made, one at a time; taps on the same d-pad key while one is on its way
      go as one press with a count, as in the Mini App. A d-pad press that
      worked says nothing: the screen is the answer.
    The owner's built apps (the app builder) are not listed in Work; they stay
    in the Telegram Mini App.
    - **Memory**: a search field over what eve remembers. At rest it shows
      the profile (what eve knows about you). A search shows three groups:
      **Facts** (the records, each with where it came from), **Remembered**
      (the meanings mem0 kept) and **Said** (the words, with the day, time and
      who). Under the results, **Forget everything matching "…"** previews the
      counts ("3 things match, in facts, what was said") and asks before anything
      goes; the forget is the chat's own two-step, everywhere at once, and the
      profile reloads after. **+** (top right) remembers something you type, as
      "remember that" does in the chat. A long press on any row offers **Don't
      mention this again** (ChatGPT's control of that name): eve leaves those
      words out of the profile and its searches, deletes nothing, and the
      **Hidden** list at the bottom of the profile takes them back with
      **Unhide**. The daemon's markdown is read into rows on
      the phone (`Shared/MemoryText.swift`): sections by heading, one fact per row
      with the day it was said, record names as words, the front matter and the
      title left out. Counts only, never the words, leave the Mac in a log.
  - An empty section says one short line ("No jobs.", "No notes, music, lights
    or TV on this Mac."). A failure says one line ("eve's chief is off.").
- **Voice mode** (`App/VoiceCall.swift`, docs/voice-mode.md): full screen,
  black, StackChan's face with captions (2026-10-09). The Action Button talks (2026-10-09: "hold to talk on the
  app should also be action not screen hold"): press to talk, press again to send. Nothing listens until it is
  pressed. A tap on the face stops eve while he thinks or speaks. The line under the face says
  "Press the Action Button to talk" when nobody is talking. The X ends the call. "Ask eve" ends the call and puts the cursor in the chat's
  composer. It replaced the Talk sheet.
- **Settings** (`App/SettingsSheet.swift`): what the old main screen had except
  Hold to talk (which is voice mode's orb): the phone's call state and End call, the pairing and link
  status, **Memory** (the card described under Apps, pushed in Settings' own navigation; shown while the Mac's
  memory is on), pairing from a pasted link, and the diagnostics log. A line at the
  top says when jobs wait on the owner.
- **Not paired:** Chat and Work both say how to pair and take a pasted pairing
  link. The app never shows an empty page.
- **Haptics:** a light tap on send, a soft one when the first words of a reply
  arrive (the page asks for it), a selection tick on the pill and the dial, and
  a success haptic when a job action goes through (Go, for example).
- **Dynamic Type and VoiceOver** everywhere: the system text styles, and a
  label (and a hint where the action is not obvious) on every control. A job
  row reads as "Book the dentist. Waiting on your Go. 1 of 2 steps done."

### What the owner sees in the chat

- eve's replies stream in as they are written. A reply can hold a table, a
  chart, a list, steps or choices, drawn by OpenUI.
- Messages from Telegram and from voice show in the same history.
- eve reacts to the owner's message the way it does in Telegram: 👀 while it
  reads a photo or file, ✍ when a note is kept or note taking starts, 🏆 when a
  fact is starred, 👍 when the words were typed to Claude or Codex. See
  "Reactions".
- Pictures and files eve sends (a photo, a screenshot, a file) show as
  eve's own message; a tap opens any picture or file full size.
- A reply that used eve's memory shows a small "Sources · N" under it; a tap
  opens the memory lines it leaned on. See "Sources".

## How it is built

Three parts. Only the middle one is a web page.

1. **The composer is native SwiftUI** (`App/ChatScreen.swift`). It is a
   `TextField` with a vertical axis, the system font, Dynamic Type and
   VoiceOver labels. Sending calls `window.eveNative.send(text)` on the page,
   so the page runs the turn through OpenUI. The typed message and the streamed
   reply then look exactly as if they were typed in OpenUI's own composer.
2. **The message list is OpenUI in a WKWebView** (`App/ChatWebView.swift`). The
   page is built by `ios/EveLink/Web` into `App/ChatWeb` and is part of the
   app. A `WKURLSchemeHandler` serves it at `eveapp://chat/index.html`. It is
   never loaded from a URL.
3. **All network is native** (`Shared/ChatTransport.swift`). The page asks the
   app through the `eve` message handler. The app calls the daemon through
   the Mini App's tunnel with the link token, and passes the answer back as
   text. The token is in the Keychain and in the `Authorization` header. It is
   never in JavaScript and never in a URL.

The page and the app talk only through these calls
(`Shared/ChatBridge.swift`):

| Page to app (`postMessage`) | What the app does |
|---|---|
| `{type:"ready"}` | Turns the composer on and shows the page. |
| `{type:"turn", id, text, client_id, attachments?}` | `POST /api/link/turn` with the speed dial's `grade` added by native, streams the reply to `onChunk`, then `onDone` or `onError`. `attachments` is up to four upload ids (32 lowercase hex each, none twice; anything else and the message is dropped); with one, `text` may be empty. |
| `{type:"cancel", id}` | Stops that turn or poll. The page hears nothing more about it. |
| `{type:"messages", id, after, wait}` | `GET /api/link/messages`, gives the JSON to `onMessages` as a string. |
| `{type:"haptic", style}` | Plays a light, soft or success haptic. |
| `{type:"openURL", url}` | Opens http(s) in Safari inside the app; mail, phone, messages, Maps and FaceTime links in their apps. |
| `{type:"openAttachment", id, name, kind}` | `GET /api/link/file/<id>`, then Quick Look with share. `id` must be 32 lowercase hex and `kind` one of image, pdf, text, doc, file, or the message is dropped. See "Attachments". |

The app calls `window.eveNative.onChunk(id, text)`, `onDone(id)`,
`onError(id, message)`, `onMessages(id, json)`, `send(text, attachmentsJSON?)`, `setBusy(bool)`,
`scrollToBottom()` and `setInsets({top, bottom})` (see "The cut-off"). Every string goes in as an escaped JavaScript literal,
so text from eve cannot run as code. A call the page has not set up yet is
skipped.

The stream bytes go to `onChunk` in order. Bytes that arrive during one turn of
the main run loop go as one call. A character split between two network reads
is held until it is whole.

The page's `{type:"turn"}` message did not change for the speed dial: native
adds `"grade": "auto" | "instant" | "think"` to the request body
(`ChatRequests.turn`).

When a call cannot reach eve, the app asks the rendezvous for eve's new
address (the same path a call uses) and tries once more. Trying a turn
again is safe: the daemon hears a `client_id` once.

## Attachments

Owner, 2026-10-08: "also allow me to attach files through app chat".

**What can be attached.** Photos, PDFs, text files (txt, md, csv, tsv, json,
log, yaml) and Word, Excel, PowerPoint or RTF documents, 20 MB each at most,
four on one message. Photos and PDFs go to the model inside the request, so together they may be 20 MB at most
on one message (`link_chat.MAX_INLINE_BYTES`); over that the turn is a 413 and the composer shows "send fewer at
once" before any model call. The daemon decides by the bytes, not the name
(`bridge/src/eve_bridge/link_uploads.py`): a photo must pass the same
check as a Telegram photo (`telegram_images.validate`: a still JPEG, PNG, WebP
or GIF, 10 MB and 25 megapixels at most), a PDF must start `%PDF-`, an Office
file must be a zip, and a text file must be UTF-8 with no NUL bytes. The app
sends every photo, HEIC included, as a JPEG of at most 2048 px on its long side,
as Telegram does.

**How a message with attachments goes.**

1. The composer uploads each one: `POST /api/link/upload` with the raw bytes,
   the link token as a Bearer, and the file's name percent-encoded in
   `X-Filename`. The same rules as every `/api/link` call: any `Origin` is
   refused (403) before the token is read, and a wrong token is 401. The body
   needs a `Content-Length` (411 without one); over 20 MB is 413 from the
   header alone, before a byte is read; a type eve does not take is 415, and
   its `error` is the line the composer shows. The answer is
   `{"ok": true, "id", "kind", "name", "mime", "size"}`; `kind` is `image`,
   `pdf`, `text` or `doc`.
2. If any upload fails, nothing is sent: the attachments and the words stay in
   the composer, with one red line saying why.
3. Native calls `window.eveNative.send(text, json)`: `json` is
   `[{id, name, kind, thumb?}]`, where `thumb` is a 240 px JPEG made on the
   phone, as a `data:` URL. The page puts them in the owner's message (AG-UI
   `binary` parts) and shows them above the bubble at once: a photo as a
   rounded thumbnail (up to 200 pt wide when it is the only one, 96 pt squares
   when there are more), anything else as a chip with an icon and the name
   (`Web/src/UserMessage.tsx`).
4. The page's turn carries `attachments: [id, ...]`, and native passes them on
   in `POST /api/link/turn`. The daemon refuses the turn (400) when an id is not
   32 lowercase hex characters, was never stored, has expired, or comes twice.

**What the brain gets.** The turn takes the path a Telegram photo with a
caption takes (`TelegramInlet._image`): with the Claude or Codex relay on, each
photo is saved in the Telegram images folder (`eve-telegram-images-<uid>`)
and its path typed to the relay, with each file's path from the upload folder;
otherwise eve's own brain answers, with each photo as vision input
(`input_image`), a PDF as the file itself (`input_file`), a text file as its
words (up to 100,000 characters), and a Word, Excel, PowerPoint or RTF document
as where it is on the Mac, so a task can open it. A message with attachments is
never taken as the answer to a waiting question: while one waits, eve asks
for the answer first, as it does for a Telegram photo.

**Where they are kept.** `<tmp>/eve-link-uploads-<uid>`: a 0700 folder this
user owns (refused otherwise), each file 0600, named by its random id only. The
owner's file name is shown, never used as a path. Uploads expire 24 hours after
they came in, like Telegram images: swept on every upload, checked on every
read. At most 200 of the owner's uploads are kept at once; eve's own copies
(below) have their own cap and never count toward it.

**In the history.** The owner's transcript line keeps each attachment's id,
name and kind (`att`). `/api/link/messages` gives an owner message its
`attachments`, with a photo's small `thumb` while the upload lives, and
`"gone": true` once it expired; the page then shows the photo as a chip with
its name. A message with only attachments and no words still shows.

**Opening one full size.** A tap on any attachment, the owner's or eve's,
posts `{type:"openAttachment", id, name, kind}`. Native fetches
`GET /api/link/file/<id>` with the link token, writes the bytes to
`tmp/chat-open/<id>/<name>` (the name cleaned to one path component,
`ChatFiles.safeName`, with the type's extension added when it has none), and
shows it in Quick Look, whose share button saves or sends it. The file is
deleted when Quick Look closes. An expired one (`gone`) is not a button; a 404
says "That file is gone. eve keeps pictures and files for a day."

The file door serves a photo or PDF inline with its own type. Anything else is
`Content-Disposition: attachment`, and a type a browser would run or render as
a page (HTML, SVG, XML, script) is sent as `application/octet-stream`
(`link_uploads.served`), whoever kept it.

**Not there.** No camera button: Photos covers it, and a camera would need the
camera permission and its own screen.

## eve's pictures and files

Owner, 2026-10-09, in the app: "It can't send attachments" / "Like images" /
"Fix". The live log at 00:14 showed an app turn run `take_photo` twice and save
the photo, but the owner never saw it: with Telegram app-only
(`EVE_TELEGRAM=app`, `telegram.MutedBotApi`) the photo went to a Bot API
that sends nothing, and the app showed only text lines.

**One door for every picture and file.** `TelegramInlet._send_media` is the only
place eve sends the owner media: `take_photo`, `screenshot` (the desktop, the
Chrome lane's page, a Codex task's tab), `send_file`, a finished task's files
and its end-of-task screen, the TV's screenshot, and a built app's picture
(`apps_maker.ChatMaker`, through `_send_app_photo`). The chief and the tasks it
runs reach these through the same methods. For each one it:

1. keeps a copy for the app (`_keep_for_app`): the bytes go into the link door's
   upload store (`UploadStore.keep`, the same folder and 24 h rule as the
   owner's uploads; `classify` picks the kind, and anything it would refuse, a
   zip for example, is kind `file`), and eve gets a `kind: "media"`
   transcript line with `att` and the caption as its text (it may be empty).
   Over 20 MB, the line names the file instead ("video.mov (31 MB) is too large
   to show here.");
2. then calls the Bot API exactly as before: with Telegram on, the phone gets
   it in Telegram; muted, the notifier posts "eve sent a picture" or the
   caption (docs/notifications.md). If that call fails and the app has the
   copy, it is one warning line, not a failure: the owner has the picture.

eve's copies have their own cap: at most 100 of them and 512 MB
(`MAX_EVE_KEPT`, `MAX_EVE_BYTES`). A new one deletes the oldest of eve's
copies to make room, never an owner upload, and they do not count toward the
owner's 200, so a busy day of screenshots never refuses the owner's upload.

A `media` line is not talk: it does not restart an open chat's ten-minute quiet
clock, so a task's or the chief's picture never puts off the idle close. When
the chat was already closed, the line opens a conversation of its own, which
closes after ten quiet minutes like any other.

The daemon lends the store to the inlet at start (`daemon.py`), and the link
door lends its own on every request (`LinkChat._attach`). Without an Eve Link
door nothing is kept, and the send is what it was.

**Not in the model's history.** The model already had the picture as its
tool's result (`take_photo`'s `shows`, the screenshot's answer), so a `media`
line is not added to the RAM history or the chat log, and no prompt block,
chat-log seed or memory index takes the kind (`transcripts.RENDER_KINDS`,
`chat_log.SEED_KINDS`, `memory_index`). The transcript `read` and `search`
tools do show it, as one line of what was said that day (its caption), so
"what did you send me this morning?" still has an answer.

**In the app.** `/api/link/messages` returns the line as an eve message with
`kind: "media"` and `attachments` (with a photo's `thumb`, or `gone`). The page
(`Web/src/sync.ts`) maps it to its own assistant message, never merged with
eve's words before or after it, and keeps its attachments beside the message
by id (`attachments.ts`, `eveMediaOf`), since an assistant message's content
is a string. `AssistantMessage.tsx` draws them above the caption, left-aligned
(`AttachmentList.tsx`: the same thumbnail and chip as the owner's, up to
240 pt wide for one photo). A tap opens it full size, as above.

**During a turn.** The stream carries words only. A picture sent mid-turn is a
transcript line, so the long poll brings it; the ledger holds it, like every
line, until the page's run settles, and never claims it as a copy of the
streamed reply, whatever its caption says. When the run settles it goes in
just before the streamed reply, where the transcript has it, as its own bubble,
with no refresh (`LedgerSink.insertBeforeReply`: the reply is taken out and put
back after the picture, both through the store's functional updates). Read back
from history later, it is in the same place. A picture that comes in after the
reply's own line, or once the stream has ended, goes after the reply, as the
transcript has it too. A picture sent with nothing streaming (a task finishing,
the chief) shows at once.

## Reactions

Owner, 2026-10-09: "can you build in reactions for when it get image or takes
not or sees text and things like that like telegram".

In Telegram eve says "got it" with a reaction on the owner's own message
instead of a line (`TelegramInlet._receipt`). An app turn had no Telegram
message to put it on, so until now the app got the line instead ("could not
react (0: no message id); said it instead"). The app now gets the reaction.

**Which ones.** The same set as Telegram, plus what the owner asked for:

| Reaction | When |
|---|---|
| 👀 | eve got a photo or file with the message and is reading it. It goes when the turn ends, or the turn's own receipt replaces it. |
| ✍ | a note went into the vault (`capture_note`), or note taking started (`take_notes` start; app turns only). |
| 🏆 | a fact was starred (`remember`). |
| 👍 | the words were typed to Claude or reached Codex. |

All four are on Telegram's ReactionTypeEmoji list. A round of only receipt
tools (`capture_note`, `remember`, and in the app `take_notes` start) ends the
turn with the reaction and no second model call, as in Telegram
(`APP_RECEIPT_TOOLS`, `TelegramInlet._receipt_of`). A `take_notes` stop or
status is news, so the model words it, and so is a start while notes are
already running (the tool's `already`). The line a reaction stands for
("Starred for good.", "Taking notes.", `Typed.`) is sent only when the reaction
cannot be set.

**How it works.** `bridge/src/eve_bridge/link_reactions.py`:

1. An app turn's id is the page's `client_id` when that is a plain id (8 to 64
   of `A-Z a-z 0-9 _ -`, a UUID from the page); otherwise a random one
   (`link_chat.turn_id`).
2. When the inlet hears the turn, it mints a negative message id for it
   (`Reactions.mint`) and puts it on the `Inbound`. Telegram ids are positive,
   so an app id is never sent to Telegram, as a reaction or as a reply link.
3. `_receipt` and `_unreact` go through `TelegramInlet._react`: an app id sets
   the reaction in the store, any other id calls `setMessageReaction` as
   before. The semantics are Telegram's: one reaction per message, a new one
   replaces the old, and "" takes it off.
4. The owner's transcript line keeps the turn id as `mid`. The store keeps the
   newest 200 reactions by that id in `<transcripts>/link-reactions.json`
   (0600), so history shows them after a restart. A 👀 is never written there
   (`TRANSIENT`) and is dropped when the file is read: it means "reading this
   now", and after a crash nothing could take it off.

**What the app gets.** `/api/link/messages` gives an owner row its `rid` (the
turn id) and, when it has one, its `reaction`. Every answer also carries
`reactions`, the full set by `rid`, so a reaction that changes on a message
already on screen reaches it. The cursor ends with `~<version>`: a long poll
whose cursor names an older version answers at once, and a reaction change
wakes a waiting poll as a new line does. The app passes the cursor back as it
got it, so native code did not change.

**On the page.** `Web/src/reactions.ts` keeps the reactions and which bubble
each `rid` is: a history row's own id, or, for a message sent from the page,
the `client_id` it was sent with (`llm.ts`). `sanitizeReaction` takes only a
short emoji string (16 UTF-16 units at most, pictographs, skin tones, the
variation selector, the keycap mark and the joiner); anything else draws
nothing. `UserMessage.tsx` shows the reaction as a small badge over the
bubble's bottom-left corner, ringed in the page background, that pops in when
it appears or changes (`Reaction.tsx`, `.eve-reaction` in `native.css`; no
animation with Reduce Motion).

## Sources

ChatGPT's memory article: "When ChatGPT uses relevant personal context,
Sources may appear below the response." eve does the same in the app
(2026-10-09): a reply that read memory shows, under it, a small muted line
"Sources · 3". A tap opens the list: one line per source, a small kind label
and when it is from, then its words.

| Kind | What it is | When |
|---|---|---|
| Fact | A record's fact line, as `<record id>: <line>`, or a whole record from `memory_read` | the fact's source date, when it has one |
| Remembered | A meaning the mem0 index recalled | the day, time and channel it came from |
| Said | A line from the transcripts, as `You: …` or `eve: …`, or a stretch `memory_read` read | the day and time |

**How it works.** `bridge/src/eve_bridge/memory_sources.py`:

1. When an app or Telegram turn starts, the daemon sets an empty list for it
   (`telegram._TURN_SOURCES`, a ContextVar, set where `_recall_seen` is
   reset). Each `memory_search` or `memory_read` the brain makes is read after
   it returns (`from_result`): the facts, the meanings and the lines said,
   taken in turn so one kind does not crowd out the rest. No repeats, at most
   6 sources, at most 200 characters each.
2. When the turn's reply is written to the transcript, the list rides on that
   one line as `src` (`[{"kind", "text", "when"?}]`). A turn that read no
   memory writes no `src`; no other line ever has one.
3. The model never sees any of it. The tool result it gets is what the Memory
   returned, and the history is the chat log's words. A test compares every
   request body, byte for byte, with and without the sources.
4. Forget reaches them: a reply whose sources quote the forgotten words loses
   those sources and keeps its text, and a forgotten line keeps none.
5. Logs carry no words. The memory tool's log line is a count, as before.

**What the app gets.** `/api/link/messages` gives an eve row `sources`, the
line's `src` sanitized again (`memory_sources.sanitize`: the three kinds only,
string fields, the same caps). An owner row never has it.

**On the page.** `Web/src/sources.ts` keeps each message's sources by id, like
eve's media, in a small store the bubble subscribes to. `sync.ts` sets them
when a history row is mapped (`toChat`), adds a merged line's sources to the
bubble it joins, and, when the long poll's copy of a reply streamed in the page
is dropped as a copy, hands its sources to that streamed reply
(`LedgerSink.attachSources`, `App.tsx`). `SourceList.tsx` draws the folded
line and the list (`.eve-sources` in `native.css`). The words are text,
never markup.

Not yet: ChatGPT's "mark relevant or not" and "delete a referenced chat" on a
source. Forget by words, in the chat or the Memory card, is the way to remove one.

## Why it does not feel like a web page

- The page is in the app. There is no load from the network and no loading
  flash: the web view stays hidden until the page says it is ready (or for 2
  seconds at most).
- No zoom, no bounce of the page itself, no tap highlight, no link previews, no
  long-press menus outside the message text, no back or forward swipe.
- The web view is clear, with the screen's background behind it (pure black
  in dark mode, `EveStyle.background`; the page's own background is
  transparent), so light and dark mode match. It fills the whole screen, under
  the top bar and the composer, and the page pads its list by what covers it
  (below).
- The web view goes nowhere. Every navigation except the bundled page is
  cancelled. A tapped link opens in Safari inside the app.
- The page cannot fetch anything itself: its Content-Security-Policy has
  `connect-src 'none'`.
- Haptics come from native code. The send haptic is the composer's; the page
  asks for one soft haptic when the first words of a reply arrive (a keep-alive
  ping is not words) and when a new eve line comes in from the long poll.
- No web loading screen and no web error box. OpenUI's "Loading conversation…"
  loader and its "Something went wrong" callout are hidden. The page retries
  the history quietly with backoff (1 s, doubling to 15 s), and the list fills
  when eve answers.
- No sticky hover. iOS keeps `:hover` on a tapped element until the next tap,
  so the build moves every `:hover` rule into `@media (hover: hover)`
  (`vite.config.ts`, `hoverOnlyWithPointer`). A tapped button does not stay
  highlighted.
- A markdown reply streams in word by word. OpenUI 0.17.0 shows a live reply
  only when its text has an OpenUI Lang fence, so the page puts an empty
  ```` ```openui-lang ```` fence in front of each live reply
  (`llm.ts`, `liveMarkdownAdapter`) and the renderer drops it (`replyParts.ts`,
  `LIVE_MARK`).
- A new line from the long poll scrolls into view when you are at the bottom
  of the chat. When you have scrolled up, the chat stays where it is.
- The keyboard pushes the composer up. A chat that was at its last message
  stays there; a chat scrolled up keeps its place.

## The cut-off (fixed 2026-10-08)

On the owner's phone the newest message sat under a fade above the composer
while the keyboard was up, and messages scrolled up under the status bar with
nothing behind them. Now:

- The web view fills the screen, keyboard area included, so the keyboard never
  resizes or scrolls it.
- Native measures the top bar's bottom edge and the composer's top edge (on the
  keyboard when it is up) on every layout change and calls
  `window.eveNative.setInsets({top, bottom})` in CSS px (points): `top` is
  the bar's bottom, `bottom` is the web view's bottom minus the composer's top
  (`HomeScreen.sendInsets`, `ChatModel.setInsets`). It is sent again when the
  page says ready.
- The page sets `--eve-inset-top` and `--eve-inset-bottom` on `<html>`
  (`insets.ts`). The list's top padding is the bar's bottom plus 8 px, so the
  first message never starts under the bar; its bottom padding is the inset
  plus 16 px, so the last message rests 16 px above the composer. The
  scroll-to-latest arrow and the typing dots move with it.
- If the list was at its end before the change, it is scrolled to the end after
  the new padding is laid out and again 320 ms later, after the keyboard's
  animation. A list scrolled up is left alone.
- OpenUI's own top and bottom fades are off; the bottom one was the fade over
  the newest message. The only fade is native, under the top bar, and ends at
  the bar's bottom edge. Behind the composer the screen's color starts at the
  card's top edge (18 pt of fade inside the card's height), so no fade covers
  the last message at rest.

## Design hook (simulator only)

Compiled only under `#if DEBUG && targetEnvironment(simulator)`; a device build
has none of it (checked: the strings `EVE_PAIR_URL`, `eveScreen` and
`eveKeyboard` are not in the device binary or its debug dylib; `eveSlash` sits beside them under the same
guard).

- Environment `EVE_PAIR_URL=evelink://pair?u=…&t=…`: pairs at launch, as
  that link would, with no dialog.
- `-eveScreen chat | apps | work | voice | settings`: opens that screen.
- `-eveGrade auto | instant | think`: preselects the speed dial.
- `-eveKeyboard YES` (with `chat`): puts the cursor in the composer.
- `-eveSlash YES`: opens the chat with "/" typed in the composer, so the / menu shows (against a daemon
  without `/api/link/commands`, the built-in list).
- `-eveControl notes|spotify|lights|firetv|memory` (with `apps`): opens that control's sheet.
- `-eveNote <vault path>` (with `-eveControl notes`): opens that note's page.
- `-eveActivity listening|thinking|speaking|waiting|notes`: puts up the call's Live Activity in that phase
  (simulator screenshots leave the Dynamic Island out; check it on a phone).

```sh
SIMCTL_CHILD_EVE_PAIR_URL='evelink://pair?u=http://127.0.0.1:8765&t=<32+ chars>' \
  xcrun simctl launch booted com.github.eve-bridge.EveLink -eveScreen chat
```

## Work: how it is built

`Shared/WorkClient.swift` and `Shared/ControlsClient.swift` make the calls, all native, with the link token in
the `Authorization` header (never in a URL), through the same tunnel and the
same one retry at a new address as the chat:

| Call | Answer |
|---|---|
| `GET /api/link/jobs` | `{"jobs": [card + "line", "steps_done", "needs_you", "actions"], "needs_you": n}`: open cards first, then the last 20 finished. 503 when the chief is off. |
| `POST /api/link/jobs` | New job. Body `{"goal", "detail"?, "client_id"?}`: goal 1 to 1000 characters after trimming, detail at most 2000, client_id at most 64; a JSON object of at most 64 kB. The words are heard as an app turn, as `POST /api/link/turn` hears typed words, led by `"Take this on as a job: "` (`link_chat.JOB_LEAD`, which names no act, so it never grounds a one-way step) and the details after `"Details: "`. The brain's own `take_on` files the card, with its parse, door floor, backbrief and every chief rule; nothing in the endpoint files or approves anything. The request waits for that turn (90 s at most) and answers `{"ok": true, "job": <one card, as GET gives it>, "said": [...]}` for the card that turn's own `take_on` filed (`TelegramInlet.app_filed`, keyed by the app turn's id; a card another turn files during the wait is never returned), or `{"ok": false, "reason", "said"}` when it filed none (`said`: eve's lines on the turn, OpenUI made plain). The same `client_id` within 10 minutes waits for the first request's turn, never a second card. 503 when the chief is off or the chat is not running; 400 or 413 for a bad body. |
| `POST /api/link/jobs/<id>/<go\|done\|drop\|raise\|happened\|didnt>` | The chief's own `approve` / `done` / `drop` / `raise_budget` / `happened` / `didnt`, and its dict: `{"ok": true, ...}` or `{"ok": false, "reason": ...}`. A Go here is the owner's yes, as "go c12" in Telegram, and may carry `{"rev": n}`. A Go on a card whose act may already have happened is refused (`ok: false`); answer `happened` or `didnt` first. |
| `GET /api/link/controls` | `{"lights": [{"name", "room"?}], "spotify": bool, "firetv": bool, "memory": bool, "notes": bool}`: which of the Mini App's Lights, Music and TV remote cards, the Memory card and Notes this Mac has. |
| `POST /api/link/controls/notes` | Notes (`link_notes.NotesPanel` over `second_brain`): `{"action": "list"}` (`notes: [{path, folder, title, preview, modified}]`, newest first, and `folders`), `{"action": "read", "path"}` (`body` under the frontmatter, `front`, `revision`, `editable`, `undo_id`), `{"action": "save", "path", "revision", "body"}` (the frontmatter kept; a stale revision is refused), `{"action": "undo", "path", "revision", "undo_id"}`, `{"action": "create", "folder", "title", "body"?}`, `{"action": "delete", "path", "revision"}` (into `.eve-trash/`, `trash_id` back), `{"action": "restore", "trash_id"}`, `{"action": "move", "path", "into"}` and `{"action": "search", "query"}`. Paths that leave the vault or enter a hidden folder are refused. 404 while the second brain is off. |
| `POST /api/link/controls/memory` | The Memory card (`link_memory.MemoryPanel`, over the chat's own `memory.Memory`): `{"action": "profile"}` (what eve knows about you), `{"action": "search", "query"}` (record facts, recalled meanings and the words said, as `memory_search` returns them, without the lines around a hit), `{"action": "forget", "query"}` (a preview: counts per store and a one-use token, never the words), `{"action": "apply", "token"}` (forgets what the preview counted), `{"action": "remember", "text"}` (the chat's "remember that": one claim starred for good), `{"action": "hide", "text"}` / `{"action": "unhide", "text"}` (*Don't mention this again*: the words are left out of the profile and every search, nothing deleted) and `{"action": "hidden"}` (`{"ok", "hidden": [...]}`). Listed as `"memory": true` only while the Mac's memory is on and built; 404 otherwise. |
| `POST /api/link/controls/<lights\|spotify\|firetv>` | That card's own `panel(body)` (`lights.Lights`, `spotify.Spotify`, `firetv.FireTV`, the same objects the Mini App's `/api/lights`, `/api/spotify` and `/api/firetv` use), and its JSON as it is. The body is the Mini App's: `{"action": "status"}` reads; `{"action": "set", "target", "power"\|"color"\|"brightness"}` for lights; `play`, `pause`, `next`, `previous`, `volume`, `transfer`, `mode` for music; the d-pad keys (with `repeat`), `power_on`/`power_off`, media keys, `volume`, `volume_change`, `mute`/`unmute`, `open_app` for the TV. 404 with `{"ok": false, "line"}` for one this Mac does not have. JSON object bodies only, 64 kB at most. |
| `GET /api/link/commands` | `{"commands": [{"command": "apps", "description": "..."}, ...]}`: the composer's / menu, the same list Telegram's menu gets (`TelegramInlet.menu_commands`: `BOT_COMMANDS`, plus /jobs while the chief is on and /america and /opentable while they are on, each before /stop). 503 when the chat inlet is not running; it does not wait for an owner to have texted. |
| `GET /api/link/today` | The home-screen widget's one read: what needs the owner, what is going, the events left today and what plays now, each source within its own limit and left out when it fails. See [the widget](widget.md). |
| `GET /api/link/apps` | `{"apps": [{"slug", "name", "updated"}]}`. Not used by Work now (the built apps are not listed there); kept for later. |
| `POST /api/link/apps/<slug>/open` | `{"url": "<tunnel origin>/apps/<slug>/?t=<app token>"}`; 404 for an app that is gone. Not used by Work now. |

- The decoding is lenient: a card without an id is left out, an action the app
  does not know is left out, and an app without a name shows its slug.
- The app sheet loads the open answer's URL only when it is on the paired
  tunnel (same scheme, host and port: `WorkApp.decodeOpen`). The web view stays
  on that origin; any other link opens in Safari.
- The menu button's dot is `needs_you`. The app reads `/api/link/jobs` when it
  becomes active and then every 60 seconds while it is active and paired, and
  after every action.
- A job id or app slug is one escaped path part, so a `/` in it cannot climb
  the path.

The home-screen widget ([widget.md](widget.md)) shows the same jobs, plus the
watches, the calendar and what plays, from its own read. A tap on it opens
Work (`evelink://work`). The app asks it to refresh when Work's jobs change.

## Why OpenUI in a web view, with a native composer

eve's replies can be generative UI: OpenUI Lang that becomes tables, charts,
steps and choices. OpenUI renders that with React. A native renderer for every
component would be a second copy of OpenUI that drifts. So the message list is
OpenUI as it is. What the owner touches most (the field, the keyboard, the send
and mic buttons, haptics) is native, so it feels like an iPhone app.

## Background and foreground

When the app goes to the background, or Work is showing (the web view leaves
the window; the page is kept, so coming back does not reload it), WebKit tells
the page it is hidden (`visibilitychange`), and the page stops its long poll. In
the background the app holds a background task for up to 25 seconds, then
stops any stream that is still open (or sooner, if iOS ends the background
task first). Without the background task, iOS suspends the app in a few
seconds and the stream stays open to the daemon with nobody reading it. When
the stream stops, the daemon sends the rest of the turn to Telegram (with
Telegram app-only, to [ntfy](notifications.md)), so the owner gets a
notification. The page gets `onError` and reads the reply from the
history when it comes back.

## What the daemon does for app turns

- Text from the app takes the same answer rules as text typed in Telegram. A
  bare "yes" or "no" answers a waiting question. A new request typed while a
  question waits (for example a chief's Go) is a new turn, and its reply
  streams to the app.
- The app has no buttons for eve's own messages. A message with buttons
  reaches the app as the words that answer them (`telegram.reply_words`):
  a question's `Reply "yes" or "no".`, and a chief message's, such as the
  `take_on` backbrief's `Reply "change c12" or "drop c12".`
  (`TelegramInlet.tell_owner`). Typed back, the words take the button's own
  path. An app turn's backbrief goes to the app's stream, not to Telegram.
  While Telegram is app-only, a backbrief from any other turn keeps its
  buttons for Telegram's place (the notifier), and its transcript line, which
  the app's chat shows, ends with the same words. The model's history keeps
  the message without them.
- When no app stream is reading a line of an app turn, the line goes to
  Telegram. Each OpenUI Lang program in it becomes one plain line, "(A table or
  chart is here: open Eve Link to see it.)" (`link_chat.strip_openui`). The
  transcript keeps the full reply for the app.
- The chat history keeps eve's reply whole, programs included. Each turn
  reads it through `link_chat.history_text`:
  - A later app turn reads the program as written. A follow-up such as "make
    it 4 days", "the second one" or a tapped button's words then has the
    block's titles, rows and options to work from.
  - A Telegram, voice or call turn reads each program as the plain line above,
    so those turns never copy the format. `recent_messages` does the same.
  - A line logged before this change holds only the plain line. An app turn
    reads it as "(A card was shown here; what it held is no longer in this
    history.)", not as "open Eve Link".
  - Program text counts toward the history's 12,000-character window, so a
    block or two pushes older lines out sooner.
- `busy` in `/api/link/messages` is false while a question waits for the
  owner, so the app does not show the "replying" dots under eve's question.
  A long poll also returns early when `busy` changes. While `busy` is true the
  page polls with `wait=2`, so the dots go away about 2 seconds after the turn
  ends.
- When the daemon stops, each open stream gets up to 0.5 s to write its
  `[DONE]` before its connection is closed.
- The 4000-character limit counts Unicode scalars on both sides (Python
  `len()`, Swift `unicodeScalars.count`).

## Using generated cards

Generated replies have a **Show text** button below the card. It opens selectable
text from the original reply; **Show card** returns to the same live card with
its inputs intact. Tables retain column labels beside each row value, and charts
retain category labels, series names, units and values. This is a static view of
supported display fields, not a transcript of live calculations or edited inputs.
It does not expose action payloads, field IDs, password input contents or program
source. Content outside the component guide may have only a partial text view.

Cards with inputs also have **Reset inputs**. This restores that card's original
defaults without sending a message. Form values survive message-list remounts in
the current page, with a bounded cache of the 100 most recently updated cards.
They are not saved to browser storage and do not survive a page reload. Each
message and block has its own state. Submitting a choice includes its current
form values in the existing conversation action. If Eve is already replying,
the card asks the owner to wait and tap again; rapid duplicate submissions are
also blocked. Opening a URL still uses the native link handler.

While a card streams, the page validates it against the same component catalog
as the renderer. A useful partial card can appear before the whole program
arrives. If a later chunk is malformed, the last usable card stays visible, with
streaming interactions disabled. Until there is a usable card, the page shows
“Building your card…”. A finished program must have a nonempty Card root, complete
syntax, resolved references and no catalog errors. Query and Mutation programs
have no tool provider in this host and use the fallback instead.

A failed final parse or reported renderer error shows a short notice and whatever
display text could be recovered, explicitly marked as potentially incomplete.
When nothing can be recovered, the notice suggests asking Eve to resend as
text. Adjacent prose, attachments and memory sources remain visible. Wide
content scrolls within the card at phone widths. Rendering diagnostics are
handled locally, without logging generated content or publishing observability.

`Web/src/GeneratedCard.tsx` hosts the card, `uiProgram.ts` checks its program and
extracts static text, and `cardActions.ts` guards outgoing choices. The component
wrapper stays mounted through streaming completion and text/card switches; only
an explicit reset restarts the renderer. Initial state is captured once per
mount, so stream updates do not reapply stale form defaults.

To include source changes in the phone app, `sh ios/EveLink/build.sh` now installs
the locked web dependencies and rebuilds the page before generating the Xcode
project. It requires Node 22.12+ and pnpm as well as Xcode/xcodegen, and stops on a
web build failure. Direct Xcode builds need `pnpm install --frozen-lockfile` and
`pnpm build` in `ios/EveLink/Web` first. Generated bundles stay out of commits.

These changes apply the catalog, progressive rendering and fallback ideas in
[OpenUI's Intelligent UI analysis](https://www.openui.com/blog/how-chatgpt-intelligent-ui-works)
and the local-input/reset pattern in
[OpenIntelligentUI's interaction contract](https://github.com/CopilotKit/OpenIntelligentUI/blob/main/docs/interactive-answers.md).
Eve continues to use its installed OpenUI runtime. Model instructions and the
component reference are unchanged, so this change requires no new prompt eval.
The reply-selection eval below remains the record for the current prompt.

## When a reply uses OpenUI

Owner, 2026-10-08: "make sure it uses openui features when its good for it to
use". In the week before, 1 of 24 app replies used OpenUI.

**Why it rarely did.** Two things in the brain's instructions:

- The persona told every text turn, app turns included, to answer in "one to
  three short sentences, plain text, no markdown, no lists". GPT-6 models
  follow format rules literally, and OpenAI's GPT-6 guide says conflicting
  instructions make them stall or fall back. The app line said the opposite,
  and the persona came first. On the dev cases, the old prompt often put a
  plan or a list of dinners into one long sentence.
- The rule for when to use OpenUI was one sentence at the very start and one at
  the very end of a 54,241-character block, which was mostly component
  signatures and five large desktop-dashboard examples.

**What an app turn reads now.** Telegram, voice and call turns are unchanged.

1. In the persona, the texting style is replaced by `telegram.APP_STYLE`. It
   keeps the voice (warm, brief, no emoji, no dashes) and leaves the form of
   the reply to the app section (`telegram.app_instructions`).
2. At the end of the stable instructions, `link_chat.app_block()` adds three
   parts in this order:
   - `APP_RULE`: a two-way decision. Use one openui-lang block when the answer
     has a shape the owner will scan: a comparison, options to pick from,
     steps, a plan or schedule, numbers for a chart, or a tap choice. Use one
     to three short plain sentences for chat, greetings, thanks, feelings, a
     single fact, a yes or no, or a question back. For a how-to or a plan,
     give the usual answer first and ask about a detail in one sentence after
     the block. The next app turn sees the block (see above), so the owner's
     answer can change it. It ends with one small, complete example reply
     (`APP_EXAMPLE`).
   - The openui-lang reference (`openui_prompt.txt`).
   - `APP_LINE`: a one-line reminder of the same decision.

**The trimmed reference.** `ios/EveLink/Web/scripts/gen-prompt.mjs` builds
the reference from the library's own spec (`generateSystemPrompt`). It keeps
only the components that suit a phone chat (`CHAT_COMPONENTS`):

| Group | Components |
|---|---|
| Layout and text | Card, CardHeader, TextContent, Callout, Tabs, TabItem, Accordion, AccordionItem, ImageBlock |
| Data | Table, Col, EntityList, Tag, TagBlock |
| Lists | ListBlock, ListItem, Steps, StepsItem |
| Charts | BarChart, LineChart, PieChart, Series |
| Choices | Buttons, Button, FollowUpBlock, FollowUpItem, Icon |
| Forms | Form, FormControl, Input, Select, SelectItem, Chips, ChipItem |

- Every type in a kept signature that names a left-out component is removed
  from it. So Card's children list only kept components.
- Library notes and rules that name a left-out component are dropped. So are a
  few that do not fit eve: "generate realistic data", "always chart
  numbers", and a templated image host.
- Six rules are added: use only facts you have, keep it phone-sized, which
  chart for what, Buttons for tap choices, real URLs only for ImageBlock, and
  Callout only for a warning.
- The library's examples are replaced by four small ones: steps, options with
  buttons, a chart from numbers the owner gave, and a comparison table.
- The two syntax rules that show a placeholder component name now show real
  components.
- The library's notes name actions in an older form ("continue_conversation",
  "open_url", an action object) and one example uses a name it never defines.
  Those are rewritten to the two action steps the library has, `@ToAssistant`
  and `@OpenUrl`. The script stops with an error if the reference names any
  other step, a snake_case identifier or an action object, and it parses the
  ListBlock note's inline example.
- Before writing, the script parses each example with the app's parser. It
  also stops with an error if the reference names a component it does not
  describe.
- `pnpm check-prompt` exits 1 when the committed file is stale.
- The app still renders every component of the library. The trim only
  changes what the brain is told.

**Cost, in characters.**

| | Before | After |
|---|---|---|
| `openui_prompt.txt` | 54,241 | 16,683 |
| The whole app block (rule, reference, reminder) | about 54,400 | 18,722 |
| An app turn's instructions, first round (dev eval) | 59,438 | about 24,100 |

The last figure is 23,927, measured in the dev eval, plus the 164 characters
the action-step and history fixes added to the block. An app turn's prompt is
now about 35,300 characters shorter, so it costs less
and is cached sooner. A Telegram turn did not change.

**Checking a program.** `node ios/EveLink/Web/scripts/validate.mjs`
(`pnpm validate`) parses an openui-lang program with the app's parser and
library (`createParser(openuiChatLibrary.toJSONSchema())`, as the Renderer
does). It exits 1 if the program:

- has no `root = Card(...)`,
- is cut off,
- uses a name it never defines, or
- has a validation error: an unknown component, a missing argument, an extra
  argument, or a wrong type.

Statements that nothing reaches are reported but do not fail it. With
`--batch`, it reads a JSON array of programs and prints a JSON array of
results. It reads stdin as a stream and lets stdout drain before it exits, so a
batch larger than the 64 KB pipe buffer works (`src/__tests__/validate.test.ts`
pipes 1 MB through it).

**Measuring it.** `bridge/tools/openui_eval.py` runs each case as a real app
turn and scores it by code:

- The inlet builds the request, with `_FROM_APP` set as `hear(app=…)` sets it.
  The reply is taken from `app_out`.
- Every tool call is refused, and Telegram is a recorder.
- The environment is made up: memory, records and web search are off, and
  only `OPENROUTER_API_KEY` is kept.
- `uses_ui`: the reply holds a complete fence or a bare `root =` program, read
  as `replyParts.ts` reads it.
- `parse_ok`: every program in the reply passes `validate.mjs`.
- It prints each case, then totals by label.
- The replies go to `--out` (or a temp file it names) before anything is
  scored. If the validator fails, the replies stay on disk and the error says
  how to score them again: `--rescore FILE`. After scoring, the file is
  written again with the scores.
- It imports only what existed at 25d2728, so a copy of the file scores an
  older checkout. Point `--validator` (or `$OPENUI_VALIDATOR`) at any
  `validate.mjs` that has its packages installed.

```sh
cd bridge
uv run python tools/openui_eval.py --cases tests/fixtures/openui_dev.json --arm after --out /tmp/after.json
```

The ten dev cases (`tests/fixtures/openui_dev.json`) were written before the
first run: six where the answer has a shape and four chat. On gpt-6-luna at
low effort, 2026-10-08:

| | Runs | Shaped cases with a block | Blocks that parse | Chat answered as plain text |
|---|---|---|---|---|
| Before (25d2728) | 5 | 17 of 30 | 15 of 17 | 20 of 20 |
| After | 4 | 22 of 24 | 22 of 22 | 16 of 16 |

- Before, the two blocks that failed to parse both used a BarChart variant
  that only LineChart takes.
- After, the two misses were both "how do I reset my wifi router". eve asked
  first whether it meant a restart or a factory reset, which erases settings.
- This dev set is small and its wording is easy. A blind set, kept apart, gives
  the verdict.


## Menu commands in plain words

The `/` menu's commands work in the app's chat in plain words too (2026-10-08). The brain has one tool for
each command that did not have one: `spend_report`, `daily_rundown`, `claude_relay`, `codex_relay`,
`set_stealth`, `open_apps` and `pair_eve_link` (`menu_tools.py`). Each tool runs the same handler as the typed
command. So "what did you spend today", "hook me into claude" or "go quiet for a while" does what `/spend`,
`/claude_on` or `/stealth` does, with the same pickers. Concerts, games and shows by place and date go to
`events_search`, which asks Ticketmaster directly (`events.py`). That tool is offered while
`TICKETMASTER_API_KEY` is set ([events](events.md)). All of this is behind `EVE_MENU_TOOLS` (on by default since
the v4 blind tool-choice eval passed on 2026-10-09; `off` turns it off). With it on, a short block of tool-choice lines also rides
the instructions: a question about a capability, a "don't ..." and something already done are answered in words; a
reservation is `opentable_search`; a movie is `web_search` (theater) or the TV (home), or one short question.
`daily_rundown` hands the message to a read-only rundown turn, and `claude_relay` never starts a new Claude Code
session itself: it sends a button. Whatever the switch says, the taint gate asks the owner before any acting tool
that follows a read of outside text in the same turn (a **Check first** question in the app's chat). See
[Text eve through Telegram](stackchan/telegram.md) for each tool and the gate, and
[the toolset router](toolset-router.md#always-carried-toolsets) for why every turn carries them.
`bridge/tools/intent_eval.py` scores these choices as app turns, with production routing.

## Claude and Codex relay in the app

"claude on" typed in the app relays the owner's words to a Claude Code session, as it did in Telegram. The app
shows both sides: the owner's relayed line as theirs, and Claude's answers (transcript `who: "claude"`,
`kind: "relay"`) as eve's messages, `via: "other"` (`link_chat._row`). Until 2026-10-08 the app hid relay
lines because Telegram showed them; with Telegram app-only, Claude's answers were logged but shown nowhere.

A relayed photo keeps the history's note in its transcript line ("why is this [image attached]"). The app finds
its own bubble among the lines that come back by the words alone, so until 2026-10-09 every photo sent with the
relay on showed twice, the second copy with the note. `link_chat._row` now drops that note (`ATTACHED_NOTE`) from
an owner line that has attachments: the app draws the attachments themselves.

## Tests

`sh ios/EveLink/Tests/run.sh` checks the bridge on the Mac: the escaping of
JavaScript literals, each message the page can send and the ones it cannot,
that every navigation to a URL that is not `eveapp://chat` is refused, which
bundled files the scheme handler serves, the requests (with each grade), and
that SSE bytes come out whole and in order, and that `setInsets` gets numbers
only. `ATTACHMENTS_OK` covers the ids a turn may carry (and the ones it may
not), the upload request and its percent-encoded name, the daemon's upload
answer, and the JSON the page gets. In `ios/EveLink/Web`, `pnpm test` checks `setInsets` on the bridge and
the pinned-to-bottom rule (`insets.test.ts`). For Work (`WORK_CLIENT_OK`) it
checks the jobs and apps decoding, the status lines, the steps done, the four
action requests and their paths, the open URL refused off the tunnel, and the
failure words. `CONTROLS_OK` checks the controls listing (only what the Mac
has, Music first), the two requests, the lights choices, the colour hex both
ways, each card's answer (artwork only from Spotify's image host), the remote's
press queue (in order, d-pad taps coalesced, the newest volume wins), and a
missing device's words. `SLASH_OK` checks when the / menu shows (a leading "/", no space), the prefix filter
and its order, the built-in 14 words, the daemon's list (bad and repeated words left out), the request and an
off chat's words. In `bridge/`, `tests/test_link_commands.py` checks the commands door (the link token, any
Origin refused, GET only, the 503 without an inlet), that its list is the inlet's (and Telegram's) with /jobs only
while the chief is on, that every base word is accepted when sent from the app (`hear(app=...)`, with
`test_telegram.py`'s menu checks), and that the app's built-in copy is `BOT_COMMANDS`. `tests/test_link_chat.py` checks the
controls door: the link token, any Origin refused, an unknown control and a
missing device as 404, the body passed to the card's panel and its answer
passed back as it is, a refused body never reaching the panel, and a panel that
raises as a 500.

`bridge/tests/test_openui_eval.py` checks the OpenUI guidance:

- The app block's order and content.
- The texting style is swapped on app turns only, robot persona included.
- The rule, its example and the reference name only `CHAT_COMPONENTS`, and
  each of them has a signature.
- The examples parse with the app's parser.
- The shipped reference is what `gen-prompt.mjs` writes.
- The eval's reading of a reply (`uses_ui`) and its scoring by label.
- A case runs as an app turn with a fake model.

The parser tests are skipped when node or the web packages are missing.

`attachments.test.ts` checks what the page takes from native and from history
(a thumbnail only as an inline JPEG or PNG), the turn's upload ids, an owner
line with only a photo, an expired photo shown as its name, and the ledger
claiming such a line. In `bridge/`, `tests/test_link_uploads.py` checks the
upload door (token, Origin, size cap, refused types), strict ids, the 24 h
expiry, the private folder, and that a photo reaches the brain as an image and
a PDF and a text file as the file and its words, with the Claude relay given
the saved paths.

`reactions.test.ts` checks `sanitizeReaction` (the receipts pass; words,
markup, spaces and long strings do not), the store (a set replaces the old
one, an unchanged set re-renders nothing, a bad `rid` or an eve row is
ignored), that a message sent from the page is linked to its `client_id`, and
the badge's markup. In `bridge/`, `tests/test_link_reactions.py` checks the
store (one reaction per message, replace and clear, an unknown id refused, the
file and its junk), that an app receipt never reaches Telegram, that an app id
is never a reply link, that a star and a note-taking start from the app end
the turn with the reaction and one model call, that 👀 sits on an app photo
while eve reads it and goes after, that Telegram's path is unchanged, and
that a reaction wakes the long poll.
eve's pictures and files: `bridge/tests/test_app_media.py` checks
`take_photo` with Telegram on and muted (the Bot API still called, the
notifier's `sendPhoto`, the app's media line before the reply, its thumbnail,
the full-size bytes, and that the line is not in the model's history), an app turn's
photo (words only in the stream, the picture once, in order), a screenshot kept
before its file is deleted, `send_file` (a PDF), a zip kept as a `file`, a file
over 20 MB as a line naming it, a built app's picture with its buttons, a
Telegram failure raised only when the app has no copy, nothing kept without a
link door, and `GET /api/link/file/<id>` (token, Origin, method, 32-hex ids
only, path climbing, expiry, and the type and disposition: inline for a photo
or PDF, a download otherwise, HTML and SVG as `application/octet-stream`),
eve's own cap (the oldest copy makes room, an owner upload is never refused
for them), a `media` line leaving the quiet clock alone, and a source scan
that no Bot API media send in `telegram.py` bypasses `_send_media`.
`media.test.ts` checks the page side: a media line as its own message with its
attachments, never merged, held during a turn and placed before the streamed
reply without being claimed, a `file` kind, an expired one
not openable, the open request, and the left-aligned buttons.
`OPEN_ATTACHMENT_OK` (`Tests/run.sh`) checks the page's open message, the
file request, and the safe local name.

Sources: `bridge/tests/test_memory_sources.py` checks a search result and a read
as sources (each kind, the caps, no repeats, a failure as nothing), that a turn
which called `memory_search` writes `src` on its reply and the next turn writes
none, that every model request is the same bytes with the feature switched off,
forget taking a quoted source and a forgotten reply's sources, and the app row
(`sources` on eve rows only, sanitized). `sources.test.ts` checks
`sanitizeSources`, mapping (an owner row has none, a merged line adds to its
bubble, a reload replaces), a claimed copy handing its sources to the streamed
reply, and the folded and open markup with a source's words escaped.

## Left out

- The Stop button stops the stream on the phone. It does not stop eve: the
  daemon has no cancel for a turn. The full reply is in the history.
- No voice notes from the composer, and no camera in the "+" menu.
- Work cannot answer a chief's question; answer it in the chat.
- No Apple push notifications (a free personal team cannot use them). eve
  notifies through the ntfy app instead: see [notifications.md](notifications.md).
  The reply itself shows in the chat when the app opens.
- The page's own text size follows the system text style the page uses. The
  app does not pass the Dynamic Type size to the page.
