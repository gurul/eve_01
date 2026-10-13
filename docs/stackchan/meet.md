# Sitting in on Google Meet calls

The owner asked on 2026-09-25 for the OpenClaw Google Meet plugin
(`@openclaw/google-meet`) and then chose to build it into eve: eve joins a
Google Meet call as a **silent notetaker**, reads the captions, and texts the
notes when the call ends.

Ask it any of these ways:

- `/meet https://meet.google.com/abc-defg-hij` (a link, a link without https,
  or a bare meeting code)
- `/meet 6pm`, `/meet now`, `/meet next`, `/meet standup`: the meeting is found
  on your calendar (the direct Calendar, [apps.md](../apps.md)) and its Meet link used
- "join my 3pm" in the chat: a model turn with the `meet_join` tool
- "hey eve, join my meeting" out loud: the voice's `join_meeting` tool

Bare `/meet` says whether eve is in a call. `/meet leave` takes it out. Both
are answered by code with no model call, and `/meet` reaches eve even while
the chat is relaying to Claude Code or Codex.

Code: `bridge/src/eve_bridge/meet.py` and the page script
`meet_page.js`. Tests: `bridge/tests/test_meet.py`, `test_meet_page.py` (the
page script in real headless Chromium against fixture pages shaped like Meet's,
including its real caption markup), and the Meet tests in `test_telegram.py` and
`test_voice_agent.py`. Live check: `bridge/tools/meet_smoke.py`.

## Listen only, for good

The owner chose a notetaker that never speaks. So:

- There is no audio path out of eve into the call at all, and no virtual
  audio device.
- The page script can click only a fixed list of controls: microphone off,
  camera off, "continue without microphone", Join now / Ask to join / Join here
  too, captions on, and Leave. A test reads the script and fails if any other
  click exists, and another runs every fixture page with an "unmute", "turn on
  camera", "Switch here" and "Present now" button beside it and checks none is
  ever pressed.
- Join is pressed only after the microphone and the camera are **seen** off.
  Meet draws the Join button before the preview's toggles, microphone on by
  default, so "no toggle yet" is not treated as off. Only when Meet shows no
  toggle for 12 seconds (no device) does eve join without one.
- In the call, if the microphone or camera comes on, eve turns it off again.
- The call is not played out loud on the Mac (`EVE_MEET_QUIET`, on). The
  captions come from Meet, not from the tab's sound.

## Your account, twice: "Join here too"

Eve joins from **your** Chrome with your Google account, so it gets straight
into your own meetings. You may be in the same call from another computer or
your phone. Meet then offers "Join here too" and "Switch here". Eve presses
only **Join here too**. "Switch here" would move the call to eve's tab and
drop your device. When Meet offers only "Switch here", eve stays out and tells
you why.

People in the call see a second tile with your name, muted with the camera off.
To have eve appear as itself, give it its own Google account signed into its
own Chrome profile and set `EVE_MEET_PROFILE` to that address. It then
usually waits in the lobby until someone admits it.

## What you get

- "I'm in <meeting>…" when it is in; "I'm waiting to be let into…" once if it
  waits in the lobby (it waits up to `EVE_MEET_LOBBY_MINUTES`, 15).
- If captions will not turn on after a minute, a text saying so.
- When the call ends, when you say leave, or after `EVE_MEET_MAX_HOURS`
  (4): the notes. A title, the gist, decisions, actions and open questions,
  with speakers' names where the captions give them. Texted under the title
  **Meet**.
- The file: `~/.config/eve-bridge/memory/transcripts/meetings/<date>/<HHMM>-meet-<code>.md`
  (mode 600), where `memory_search` finds it. The transcript is appended as the
  call goes, so a crash keeps what was heard; at the end the notes are written
  above it.

The chat's history keeps a line saying the notes were sent, not the notes
themselves: what someone said in a meeting is never read back to eve's brain
as its own words.

## How the captions are read

Meet's captions carry who said what, cost nothing, and need no audio capture.
The page script records what the caption region shows each time it changes. In
Python, `CaptionMerger` turns those snapshots into lines. That is the hard part:
Meet shows a window of a speaker's turn that grows, scrolls, and **rewrites
words anywhere in it** as it hears more. On the first live test, "He will most
likely" became "We will most likely", and "it's quite" became "it's. It's
quite". So consecutive readings of a block are aligned word by word (difflib),
and when enough of them agree the latest reading wins. When too little agrees,
it is a new line.

Captions are English only for now: eve opens every link with `hl=en`,
because the page script reads Meet's English labels.

## Where it comes from

The page script is a port of OpenClaw's google-meet plugin (MIT, Copyright (c)
2026 OpenClaw Foundation): its button labels, the lobby, ended and sign-in
wording, and the caption region selector. Its talk-back half (BlackHole, SoX, a
realtime voice) was left out on purpose. Its caption bookkeeping was replaced by
`CaptionMerger` after the first live call.

## Switches

| Variable | Default | What it does |
|---|---|---|
| `EVE_MEET` | on | Needs the Telegram door and `EVE_BROWSER_ATTACH=1` (your Chrome) or `EVE_BROWSER_OWN=1` (eve's own Chrome). |
| `EVE_MEET_PROFILE` | the Chrome lane's profile | Google account whose Chrome profile joins. |
| `EVE_MEET_LOBBY_MINUTES` | 15 | How long to wait to be admitted. |
| `EVE_MEET_MAX_HOURS` | 4 | A call is left after this long. |
| `EVE_MEET_QUIET` | on | Mute the call's sound on the Mac. |
| `EVE_MEET_SUMMARY_MODEL` | `gpt-6-luna` | The notes' summary model (spend: "meet notes"). |
| `EVE_MEET_GUEST_NAME` | Eve | The name typed in only when Chrome is signed out. |

Chrome's "Allow remote debugging?" for the Meet tab's connection is answered the
same way as the Chrome lane's (`EVE_CHROME_ACCESS=allow` presses it). In
eve's own Chrome (`EVE_BROWSER_OWN=1`) there is no dialog, and Meet joins
as the account signed in there. With `EVE_BROWSER_STEEL=1` the other web tasks run in Steel,
but Meet still joins from eve's own Chrome: Steel's Chrome is headless and muted
([Steel](../steel.md)).

## Live test, 2026-09-25

Two runs against a real 6pm test meeting on the owner's personal calendar
(`tools/meet_smoke.py 6pm`). The calendar lookup found it. Eve joined with the
microphone and camera off. The click log both times was exactly `mic-off,
camera-off, join, captions-on`. The notes named the decision ("ship the deck on
Friday") and the speaker. The first run showed the caption markup was not what
the fixtures assumed: the name and the words are two parts of one block, and the
words arrive as many text nodes. The second run showed Meet's rewrites
duplicating lines. Both are fixed, and the fixtures now use Meet's real markup.
Not yet seen live: "Join here too" (both runs got a plain "Join now").

Meeting summaries and room-note summary/reduce calls use Luna at low effort.
Transcription models are unchanged. See the [model role table](../openrouter.md#models-by-role).
