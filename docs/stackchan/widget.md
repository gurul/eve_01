# eve's diary — macOS desktop widget and diary window

A native WidgetKit widget that shows eve's newest thoughts and how it feels
about them on the macOS desktop (macOS 14+, desktop widgets), and a diary window
with everything behind them. It lives in `widget/` as an Xcode project generated
by [xcodegen](https://github.com/yonaskolb/XcodeGen).

**The widget** is a dark card: eve's name, its current feeling (emoji, label and
a colour dot — hue from valence, brightness from arousal, the same mapping as the
robot's LEDs), then the newest thoughts, each with its time and a thin colour bar
for the feeling it was written in; the large size adds what changed, and the
extra-large size shows twice the thoughts. **Tap it** (`stackchan://chat`) and the
helper opens the chat window.

## The chat window

The chat window shows eve's conversation with you, like the Telegram chat. Your
messages are on the right, eve's are on the left, and voice lines are marked 🎙.
It includes what you text on Telegram and what you say with the Voice PE's hold to
talk. Type in the box at the bottom and press Return: the words go to the same brain
as a Telegram text, with every tool. eve answers in the window, and in Telegram
as usual. The window is also in the menu, as *Open chat* (⌘T).

The window reads eve's transcripts in place
(`~/.config/eve-bridge/memory/transcripts/<day>.jsonl`, the last three days)
and checks them every 1.5 s. It never copies them: the owner's rule (2026-09-23)
keeps the words out of backed-up folders, and the widget's App Group container is
one. So the widget shows eve's thoughts, not the chat. The reply box sends
`{"evt":"chat","text":…}` on the daemon's socket (`/tmp/eve-bridge.sock`,
mode 0600, so only your own programs can use it). The Telegram chat must be set up,
and you must have texted eve there once, so it knows which chat is yours.

## The diary window

The helper's menu opens the diary window (*Open diary*, ⌘D, or `stackchan://diary`):

| Tab | What |
|---|---|
| **Talking** | what eve heard: the claims you promoted by saying *"remember that"* pinned at the top, then the dream journal of today and yesterday — what the day was about and any **debt of eve's own** (`eve owes …`) |
| **Notes** | recordings eve made of the room on request ([voice.md](voice.md#taking-notes-on-the-room)), each with *Open*, *Save a copy…* and *Show in Finder* |
| Thoughts | every thought by day; click one for the observations, what changed, tags, novelty and importance, and valence/arousal gauges; a toggle shows the unwritten candidates eve kept in memory only; **★ stars** a thought into `highlights.md` |
| Feelings | valence and arousal per thought over time (Swift Charts), a tally of feelings |
| Profile | ★ never forget (the starred layer), then eve's ROOM / HUMAN / SELF / RULES blocks |
| Dreams | the nightly insights per day, with the ★ candidates eve proposed |


## What it is

Two targets, one app bundle:

| Target | Kind | Sandboxed | Bundle id |
|---|---|---|---|
| `StackChanNotes` | SwiftUI menu-bar helper (`LSUIElement`, no Dock icon) | no — it must read `~/.config/eve-bridge/notes` | `com.github.eve-bridge.StackChanNotes` |
| `StackChanNotesWidget` | WidgetKit extension, small / medium / large | yes (required for widgets) | `com.github.eve-bridge.StackChanNotes.Widget` |

Both share the App Group `SJ8BKXTNUS.com.github.eve-bridge` (macOS App
Groups are prefixed with the Team ID, which is the `OU` of the "Apple
Development" signing certificate).

Sizes: small shows 1 thought card; medium 2 cards of up to two lines each (1
when a debt line is showing), with the chat and lessons pills shrinking to
icons before the name is ever cut; large 3 with a "changed:" line, one fewer
each for a debt card and a lead photo; extra large 6 by the same rule. Empty
state: *Nothing noticed yet — eve explores when Claude is idle.*

## The two provenances

eve remembers two different kinds of thing, and the widget never merges them:

| | What eve **saw** | What eve **heard** |
|---|---|---|
| Written by | `diary.py` | the transcripts, then the nightly dream (`dream.py`, `records.py`) |
| Lives in | `~/.config/eve-bridge/notes/` | `~/.config/eve-bridge/memory/` ([memory.md](memory.md)) |
| Shown in | Thoughts, Photos, Feelings, Profile, Dreams | **Talking** and **Notes**, and one line on the card |
| Quotable as fact | no — a camera suggested it | yes — the owner said it |
| Starred by | ★ in the diary window (`notes/highlights.md`) | saying *"remember that"* out loud or in Telegram (`memory/records/starred.md`) |

The card shows the newest `eve owes …` line above everything eve saw, because
it is the one line about the owner rather than about the room, and it is the thing
eve has not done yet.

The widget only reads the memory folder, never creates it: the daemon's one-time
move of the old store skips a target that already exists, so a folder made by the
widget would strand the owner's records. The Talking tab reads
`records/starred.md` (the same claims eve starts every conversation with) and
`records/days/<day>.md` (the dream journal, written the night after); the Notes
tab reads `transcripts/meetings/`. It never reads a transcript.
`EVE_MEMORY_DIR` overrides the memory folder, as `EVE_NOTES_DIR` does
the notes one.

## How notes flow

```
eve-bridge daemon (diary.py)
  └─ ~/.config/eve-bridge/notes/
       YYYY-MM-DD.md   "- HH:MM yaw=+20 pitch=40 — <thought>" lines + a "## Dreams" section at night
       memory.jsonl    one record per frame: thought, observations, changed, tags, novelty, importance,
                       written, valence, arousal, label
       profile.md      ROOM / HUMAN / SELF / RULES
       highlights.md   "- ★ <claim> — starred <date>"  (written by the diary window, read by eve)
           │  (DispatchSource on the dir + on the newest day file, plus a 30 s timer)
           │  it rewrites whenever ANY part of the snapshot changed, compared as
           │  one content key. Comparing a hand-listed set of fields is how room
           │  notes stayed invisible for an afternoon: they were read, held for
           │  the diary window, and never written out (2026-09-11)
           ▼
StackChanNotes.app  (helper, unsandboxed; hosts the diary window; claims stackchan://)
  └─ ~/Library/Group Containers/SJ8BKXTNUS.com.github.eve-bridge/notes.json
       notes, thoughts (newest 200), profile, reflections (dreams), highlights → WidgetCenter.reloadAllTimelines()
           ▼
StackChanNotesWidget.appex  (sandboxed)
  └─ TimelineProvider reads notes.json; refreshes on its own every 15 min as a fallback; .widgetURL(stackchan://diary)
```

Parsing lives in `widget/Shared/NoteStore.swift`: `NoteLine.parse` for diary
lines (em dash, with ` – `, ` -- `, ` - ` fallbacks; a line without a pose is
fine), `NoteLine.reflections` for the `## Dreams` (or `## Evening reflection`)
section, tolerant `Thought` decoding for `memory.jsonl` (missing fields default),
`NoteStore.readHighlights` / `NoteStore.star` for the starred layer. Older
`notes.json` files without the new fields still decode.

Environment knobs on the helper:

- `EVE_NOTES_DIR` — override the notes directory (default `~/.config/eve-bridge/notes`).
- `EVE_MEMORY_DIR` — override eve's memory folder (default `~/.config/eve-bridge/memory`).
- `EVE_NO_LOGIN_ITEM=1` — skip the one-time login-item registration (smoke tests from a build dir).

## Build and install

Requirements: Xcode 26.x, `brew install xcodegen`, an "Apple Development"
signing identity for team `SJ8BKXTNUS` (edit `DEVELOPMENT_TEAM` and the App
Group prefix in `widget/project.yml` + `widget/Shared/NoteStore.swift` for
another team).

```sh
cd widget && xcodegen generate && cd ..        # regenerates StackChanNotes.xcodeproj
xcodebuild -project widget/StackChanNotes.xcodeproj -scheme StackChanNotes \
  -configuration Release -derivedDataPath widget/build \
  CODE_SIGN_STYLE=Automatic DEVELOPMENT_TEAM=SJ8BKXTNUS build

ditto widget/build/Build/Products/Release/StackChanNotes.app ~/Applications/StackChanNotes.app
open ~/Applications/StackChanNotes.app          # first launch: registers login item + widget
pluginkit -m -v -p com.apple.widgetkit-extension | grep -i stackchan   # widget is known
```

The helper shows a small `note.text` icon in the menu bar with the note count,
last sync time, a *Microphone* line and switch, a *eve* line and its
*Turn eve off* / *Turn eve on* button, *Refresh now*, *Open notes
folder*, a *Launch at login* toggle, and *Quit*.

The *Microphone* line says what the Mac mic is doing (listening, closed because
the robot is not connected, off by your choice, or daemon not reachable). The
switch is the owner's mic switch: off closes the mic until you turn it back on,
across restarts, the same as `eve-bridge mic off`. It talks to the daemon
over its IPC socket (`/tmp/eve-bridge.sock`), so it needs the daemon
running; the WidgetKit widget itself is sandboxed and only shows notes.

## The power switch

eve is the bridge daemon that runs in the background on your Mac, and both
the desktop card and the menu have a switch for it. On the card it is the
**TURN OFF** pill (under the robot face on the medium size, in the header on
the small and large sizes). In the menu it is the *eve* line, which says
where eve is (`on (pid …)`, `off (your choice)`, running from a terminal,
or not installed as a service), and the **Turn eve off** / **Turn eve
on** button under it.

**Off stops the daemon and keeps it stopped:** the robot goes quiet, the mic
closes, no exploring, no diary, and it stays off across logins until you turn
it on. The card sleeps meanwhile (`eve is off.`, the face says *zzz*, and
the pill reads **TURN ON**). That is the difference from quitting the
process: the daemon is a launchd agent with `KeepAlive` on
(`eve-bridge install --service`), so a plain kill only respawns it.
Turning off runs `launchctl bootout` then `launchctl disable` on
`gui/<uid>/com.github.eve-bridge.daemon`; on runs `launchctl enable`
then `launchctl bootstrap` (the same override `install --service` clears
with `load -w`). After a flip the helper waits for launchd to settle (the
agent still prints as running, without a pid, for a moment after a bootout)
before it reports. A daemon you started by hand from a terminal
(`eve-bridge daemon`) is not launchd's to stop, so the switch hides and
the menu says to stop it there.

**How the card does it, being sandboxed.** The card cannot run `launchctl`,
so its pill is an interactive-widget button (`SetEvePowerIntent`, an
App Intent) whose only job is to write `power-request.json` (`{on, at}`)
into the App Group container. The helper watches that directory
(`PowerRelay`), does the launchctl work (`EveService`), and writes
`power.json` (`{on, switchable, line, at}`) back, then reloads the card. A
request stamped after the state is one the helper has not answered yet, so
the card shows **STOPPING…** / **STARTING…** until the helper's state
overtakes it, and **WAITING** (with a line saying to open StackChan Notes)
if twenty seconds pass with no answer, which means the helper is not
running. The helper also re-reads launchd every 30 s (and the menu every
5 s while it is open) so `power.json` stays true when eve is stopped some
other way. The card's pill only appears once a helper that knows about power
has written `power.json`; a Mac with no service, or the daemon run by hand,
gets no pill. Files: `widget/Shared/EvePower.swift` (the two JSON files),
`widget/StackChanNotes/EveService.swift` (launchctl),
`widget/StackChanNotes/PowerRelay.swift` (the watcher), the intent and pill
in `widget/StackChanNotesWidget/StackChanNotesWidget.swift`.

## Add the widget to the desktop (manual)

**What went wrong on 2026-09-21, and what the card does now.** An off press was served in a second, and
then no on press ever reached the helper: the daemon sat disabled and the robot showed "No Claude
connected". The helper was fine in both directions when handed the same request files. The card was the
weak half, in three ways, all fixed:

- *The pill had nothing to press while it waited.* `perform()` wrote the request and returned at once, so the
  card was redrawn as "stopping…", and the redraw that replaces that with **TURN ON** depended on the helper's
  `reloadAllTimelines()` — which WidgetKit rations for a background menu-bar app. The intent now waits for the
  helper's answer (`EvePower.answerWait`, 8 s; the helper takes about one). The redraw that follows an
  intent is part of the press and is never rationed, so the card shows the result of the press.
- *"waiting" was a dead end.* A request unanswered after 20 s drew a plain "waiting" pill for ever. It is now
  a **try again** button that sends the same request with a fresh stamp.
- *A press could vanish.* Stamps are whole seconds, and the helper serves a request only when it is newer than
  its last state, so a press in the same second as that state was dropped without a trace (reproduced by
  writing such a file: eve stayed on). A request is now stamped at least a second past the state on file, and
  the helper never stamps its answer before the request it answers.

The helper also ignores a request older than two minutes (`requestMaxAge`): pressed while the helper was not
running, it would otherwise turn eve off whenever the helper next started. And after a turn-off it waits
until the daemon has stopped answering as well as left launchd, so it never reports "running from a terminal"
— a state with no button — for a daemon that is merely shutting down.

WidgetKit does not let an app place its own widget. Once the helper has been
launched from `~/Applications`:

1. Right-click the desktop → **Edit Widgets…**
2. Search for **StackChan notes** (or scroll to *StackChanNotes*).
3. Drag the small, medium, or large size onto the desktop. Done.

If it does not appear in the gallery, re-run `open ~/Applications/StackChanNotes.app`
and check `pluginkit -m -p com.apple.widgetkit-extension | grep -i stackchan`.

## Smoke test without the robot

```sh
mkdir -p /tmp/notes
printf -- '- 10:02 yaw=+20 pitch=40 — a mug on the desk, still steaming\n' > /tmp/notes/2026-09-05.md
EVE_NOTES_DIR=/tmp/notes EVE_NO_LOGIN_ITEM=1 \
  widget/build/Build/Products/Release/StackChanNotes.app/Contents/MacOS/StackChanNotes &
cat ~/Library/Group\ Containers/SJ8BKXTNUS.com.github.eve-bridge/notes.json
echo '- 10:15 yaw=-10 pitch=30 — the window blind is half down' >> /tmp/notes/2026-09-05.md
sleep 1; cat ~/Library/Group\ Containers/SJ8BKXTNUS.com.github.eve-bridge/notes.json   # now two entries
pkill -x StackChanNotes
```

## Remove

```sh
# menu bar → Launch at login off, then Quit — or:
pkill -x StackChanNotes
rm -rf ~/Applications/StackChanNotes.app
rm -rf ~/Library/Group\ Containers/SJ8BKXTNUS.com.github.eve-bridge
defaults delete com.github.eve-bridge.StackChanNotes
```

Deleting the app removes the login item (System Settings → General → Login
Items & Extensions lists it under "StackChan Notes" if it lingers). Remove the
widget from the desktop via right-click → *Remove Widget*.
