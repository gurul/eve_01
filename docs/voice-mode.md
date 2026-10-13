# Voice mode in Eve Link

Voice mode is a full-screen view for a call with eve. It is native SwiftUI,
with no web view. It is black, with StackChan's face in the centre, captions
under it, and one row of controls at the bottom. The owner asked for it to be "very simple minimal very
very clean".

Status: wired on 2026-10-08. The top bar's voice button (top right of Chat and
Work) opens it, and it replaced the Talk sheet. Built and checked in the
simulator; not yet tried on the owner's iPhone.

## What the owner sees

- A black screen with a light status bar.
- **StackChan's face** (since 2026-10-09; owner: "can the hold to talk be the
  stackchan face with captions"). Two rounded white eyes, 74 × 92 pt with a
  46 pt gap, as the robot drew them with FluxGarage RoboEyes
  (`firmware/claude_pet_stackchan/src/eyes.cpp`). It replaced the cloudy orb.
  - Nobody speaking: the eyes blink every 2.6 to 6 s and glance about now and
    then.
  - The owner holding to talk: the eyes open a little wider, look at the owner
    and pulse slightly with the voice.
  - eve thinking (or writing up notes): the eyes squint, look up and to the
    side, and dart.
  - eve speaking: the eyes bounce with his voice.
  - Muted: half shut and looking down. Connecting: half open.
- **Captions** under the face. What eve heard the owner say appears in grey
  as soon as it is transcribed. Each sentence of eve's reply appears in white
  when its sound starts to play, and fades 4 s after his voice stops. The daemon
  sends each sentence as `{"type": "said", "text": …}` just before its audio
  (`phone_call.read_out`), and `VoiceLevels.eveSaid` holds it on the playback
  clock until that audio is heard.
- **The Action Button** talks (owner, 2026-10-09: "hold to talk on the app
  should also be action not screen hold"). It runs `TalkIntent`, which presses
  the same `Relay` voice mode shows, so the face and captions follow the turn:
  a press opens the microphone, a second press sends (or the turn ends itself
  after a pause), and a press while eve speaks cuts him off. iOS reports only
  the press, never its release, so it is press-to-talk rather than a hold.
  Holding the screen no longer talks (until 2026-10-09 the face, and before it
  the orb, was held). **Tap the face** to stop eve while he thinks or speaks,
  or to stop notes; a tap never opens the microphone. Voice mode is still not
  an open microphone (owner, 2026-10-08: "talk should be hold to tlak not
  realtime").
- **Top left**: a round menu button. It opens Settings over voice mode.
  **Top right**: a round button that opens the voice settings: one choice,
  **Speaker** (the default) or **Earpiece**. Headphones and AirPods play eve
  whenever they are connected. The choice is remembered (`voice.earpiece`) and
  applies only while voice mode is open; Action Button calls stay on the
  speaker.
- **Bottom row**, 48 pt high, set in from the screen's edges:
  - A capsule that holds "+" (reserved, disabled), "Ask eve" and the speed
    dial. "Ask eve" closes voice mode and opens the chat composer. The speed
    dial opens a menu: Auto, Instant, Think. It is the chat's stored choice
    (`chat.grade`): changing it here changes it in Chat too. The call does not
    send the grade to eve yet; it is used by typed turns.
  - A round mute button: "mic" when the microphone is live, "mic.slash" when it
    is muted.
  - A round white button with an "×". It ends the call and closes voice mode.
    "Ask eve" also ends the call.
- **A status line** above the bottom row, in muted grey: "Press the Action Button to talk" (nobody
  talking, eve quiet), "eve is thinking…", "eve is speaking", "Muted", "Taking
  notes", or an error (why the call ended, or "Pair Eve Link first…").
  "Listening" shows for 1.6 s and then fades out, because it is the normal
  state. `VoiceStatus.text` (Shared/PhoneTurn.swift) maps the call to the line.
- **Haptics**: a soft tap when eve starts speaking, a light tap on mute, a
  medium tap on close.
- **eve's expressions** (since 2026-10-12): while eve rests, thinks or
  speaks, the eyes take the shape the StackChan board drew for what is being
  said (a smile, a droop, a frown, a wink…) for a few seconds
  ([Expressions](#expressions)).
- **VoiceOver**: every control has a label. The face reads as "eve" with
  its mood ("listening", "thinking"…) and any expression ("speaking, happy"),
  and each caption is read as "You: …" or "eve: …".
- **Reduce Motion**: the eyes do not blink, glance or bounce. Only their
  openness follows the call. An expression's shape changes at once, with no
  easing, and holds its end pose: no bounce, sway, nod or drift, and its marks
  (z's, "?", sweat drop, sparkles, blush) shown still.

## The hands-free call (not used by voice mode since 2026-10-08)

Voice mode no longer calls `Relay.beginHandsFree()`: opening it only asks for
the microphone, an Action Button press is a turn (`PhoneLink.press`),
and closing it hangs up if a call is going (`Relay.endPhoneCall`). The
hands-free code below is kept, unused, in `Relay`.

Voice mode uses the call path the Action Button uses (`Relay`), with no new
daemon code:

1. Opening voice mode calls `Relay.beginHandsFree()`. If no call is going, or
   the call is quiet, it starts a phone turn at once. If eve is thinking or
   speaking, or a turn or notes are already open, it waits for them
   (`HandsFree.startsNow`).
2. The turn is open, but the phone sends nothing yet: it holds the last 0.5 s of
   the microphone (`SpeechOnset`). It judges 10 ms frames, not the
   microphone's 40 to 100 ms chunks. When 150 ms of frames in a row are louder
   than the end-of-speech threshold, it sends `talk`, the held 0.5 s and then
   the live audio. So silence, a click, a clap or a cup set down never becomes
   a turn, and the first syllable is kept.
3. The turn ends itself after a 1.2 s pause, as an Action Button turn does.
4. When eve says `listening` (his reply is over, or he could not make the turn
   out), the microphone opens again by itself (`HandsFree.reopens`). eve
   sends his reply faster than it plays, so the phone first waits for its own
   queued audio to finish (`Relay.playsUntil`, plus 0.25 s): the status stays
   "eve is speaking" until then, and a reopen never cuts the end of his
   reply. Only a barge-in (a tap on the face, or an Action Button press) drops that audio. While
   the app is in front, the call does not hang up after 45 quiet seconds.
5. **Mute** (`Relay.muted`): a muted microphone sends nothing and shows nothing
   on the face. That includes notes. A turn that has already gone out to eve
   counts the muted time as silence, so it still ends after the 1.2 s pause and
   eve answers what was said before the mute.
6. **Locked or in the background**: the call keeps going, as a voice call
   does, but the 45 s quiet hang-up is on again (`Relay.handsFreeBackgrounded`,
   set from the screen's `scenePhase`). An open microphone that hears nobody
   for 45 s ends the call, and voice mode closes. A sentence in progress is
   never cut.
7. **The phone's audio stops** (Siri, a phone call, a route change that fails)
   while the microphone waits for speech: the waiting turn is dropped (eve was
   never told of it), the status says so, and the microphone opens again when
   the interruption ends, or on a tap of the face.
8. **X** or **Ask eve** calls `Relay.endHandsFree()`, which is End call.

Outside voice mode nothing changed: `handsFree` is false, so the Action Button,
evelink://talk and the Live Activity work as before. A press of the Action
Button while voice mode listens and nobody has spoken does nothing (there is
nothing to send yet).

Not done: barge-in by voice. While eve thinks or speaks nothing from the
microphone goes to eve or to the face; tap the face to cut him off.

## Expressions

Owner, 2026-10-12: "can u route the expressions into the eyes". The daemon's
expression picker (`live_expressions.py`, Jev by default; the
[live expressions](stackchan/laya-expressions/README.md) page has its
measurements) now feeds this face, which the retired StackChan board used to
show.

**The bridge's side.**

- The app's hello lists `"eyes"` (`{"token": …, "can": ["notes", "eyes"]}`).
  Only such a call is fed or sent anything (`Call.can_eyes`); an older build
  sees no change.
- What the owner said (after `heard`) and each sentence eve reads out are
  offered to a picker of the phone's own (`Daemon._start_phone_expressions`).
  The offer is a cheap call on the voice loop; the model runs on its own
  thread, so a slow or failed pick never holds up a reply.
- **A sentence is offered when the phone starts to play it**, not when its
  audio is made. Speech is made faster than it plays, so the call keeps an
  estimate of when the phone's queued audio ends (`Call.play_end`: the bytes
  sent at 24 kHz) and offers each sentence at its start
  (`Call._offer_when_heard`). The picker's 1.2 s gap and newest-wins rule then
  run on playback time, so a fast reply no longer offers every sentence in the
  first second and shows the last one's face over the first one's words. A
  barge-in (`interrupt`) and the call's end cancel the offers still waiting.
- One pick at a time: the newest turn wins, at most one is sent every 1.2 s,
  and a pick more than 4 s after its words is dropped. A pick that fails or
  times out sends nothing.
- Nothing is sent when no Eve Link call is live, while the owner holds the
  button (`Call.talking`), or when the socket has closed
  (`PhoneCalls.eyes_call`).
- **The switch.** On by default. `EVE_PHONE_EXPRESSIONS=0` (or `false`, `off`,
  `no`) in the env file turns it off. It is separate from the board's
  `expressions.json`. The phone's worker loads Jev at its first offer, so a
  Mac that is never called from the phone makes no Jev call for it.

**The message**, one JSON text frame on the call's WebSocket:

```json
{"type": "expression", "id": 3127894523, "label": "happy", "ttl_ms": 3600, "said": "And it's green now!"}
{"type": "expression", "id": 3127894524, "clear": true}
```

- `id`: a nonzero uint32 that only grows (seeded from the clock, so it grows
  across daemon restarts; it wraps at 2³²).
- `label`: exactly one of calm, happy, curious, affection, surprised, sad,
  worried, skeptical, frustrated, excited, wink, laughing, sleepy, confused,
  sheepish, relieved, proud, flattered (`eye_model.LABELS`; the last seven
  since 2026-10-12, `docs/expressions-design.json`. The design's eighth,
  thinking, failed its blind bar and was dropped; the face's own thinking mood
  covers eve working on an answer).
- `ttl_ms`: 500 to 4000, the board's 4 s less the time the pick took.
- `said`: on a pick for one of eve's sentences, that sentence, exactly as its
  `said` caption message carried it (`Call.said_for`). The owner's words and a
  clear have none.
- `clear`: the worker was switched off; drop what is showing.

**The app's side.**

- `CallSocket` decodes the message with `FaceExpressionRequest.decode`
  (`Shared/FaceExpression.swift`), by the board's rules (`data.h`): the id must
  be a nonzero whole uint32, the label one of the eighteen, `clear` a real
  boolean, a missing `ttl_ms` means the label's own time (4000 for the board's
  eleven; laughing 3000, sleepy 5000, confused and relieved 3500,
  the rest 4000), and `said` is kept only if it is a
  non-blank string. Anything else is dropped. An
  older build ignores the message (`default: break`).
- `Relay` hands it to `VoiceLevels.showExpression` while the phone has the
  call. `FaceExpressionState.accept` takes it only if its id is newer than the
  last one taken (an int32 wrap-around compare, as `expression.h`), checked on
  arrival. It clamps the ttl to 0.5 to 6 s.
- **It starts with its sentence.** A pick whose `said` matches a caption still
  queued to play starts when that caption does (the playback clock eve's
  captions use, `eveClock`); anything else starts on arrival. Its ttl runs from
  that start. Until then the shape already showing holds
  (`FaceExpressionState.timeline`). A newer pick that starts sooner drops one
  still waiting; a clear ends what shows and drops what waits; eve's flush
  (`eveFlushed`) drops what waits, since those sentences will not play.
  `VoiceLevels.expression` is the shape until its ttl has run, and the meter
  keeps ticking until then, so it starts and expires on time. The end of the
  call (`reset`) clears it and the id order.
- **When it shows.** Only while eve rests, thinks or speaks
  (`FaceExpressionState.allowed`). It never shows while the owner talks (the
  face is listening), when muted, or while connecting. A shape that is still
  live shows again once the owner stops.
- **The shapes** are the board's `expression.h` style table, unchanged, and
  their heights are the eye: the board opens the eyes and sets the style's
  heights over the agent face, so while an expression shows the mood's
  openness (thinking's squint, speaking's 0.9 to 1.12) eases back to 1. Only
  the blink and a small bounce with eve's voice (up to 10%) remain. The
  board's eye at rest (96 px tall and wide, 22 px corner) stands for the
  phone's rest eye (74 × 92 pt, 24 pt corner), so each number becomes a ratio:
  each eye's height, the width, the corner, the blink pace (every *blink* to
  *blink* + 1 s) and curiosity. The lids are RoboEyes':
  - **smile** (happy, affection, excited): a rounded lid pushed up from below
    cuts away the bottom half, so the eyes curve up.
  - **droop** (sad, worried): each eye's outer top corner pulled down.
  - **brow** (frustrated): each eye's inner top corner pulled down.
  - **curious** (curious, affection, worried, skeptical): the eye on the side
    the pair looks to grows by 8 board px.
  - **wink**: once per id, the viewer's right eye shuts for 650 ms. When it has
    collapsed, a curved closed lid replaces it, and a lifted brow shows over the
    open eye (`FaceWink`, from `eyes.cpp`). There are no blinks or curiosity
    during the wink.
- **The seven of 2026-10-12** (`docs/expressions-design.json`, "shape") add
  two lids, motion and marks. Their timing is pure math in
  `FaceExpressionLabel.pose(at:)` (seconds since the label showed, in board
  px), which `FaceMotion` eases and `StackChanFace` draws:
  - **flat** (sleepy, sheepish): a level upper lid. The top comes
    down with a small 7 board px round at its ends, so it reads as a lid and
    not a cropped box; the bottom stays where the rest eye's is.
  - **droop + smile** (relieved): the outer droop at half of sad's depth over
    a smile at 70% of happy's.
  - A smile can be deeper or lighter than happy's: laughing 1.4 (70% cover,
    and the eye's top and the lid arch together into an even crescent), proud
    0.5, flattered 0.8.
  - **laughing**: snaps into its crescent in about 120 ms (the others ease in
    over about a third of a second), then both crescents bounce 4 px at 5 Hz
    for 1.2 s, then 2 px at 2 Hz. No blinks.
  - **sleepy**: 40 px slits; every 2.5 s the lids sag to 24 over 600 ms and
    pop back; the gaze sinks 6 px; 400 ms blinks every 6 s; two white z's (14
    and 20 px, the second starting 16 px further right) rise and drift right
    over the right eye, 400 ms apart, so both show together for most of the nod.
  - **confused**: the tilt swaps (104/80, then 80/104, 500 ms each, twice)
    with an 8 px gaze sweep, then rests at 100/88; a 22 px "?" pops in over the pair
    at 200 ms and tips 10 degrees toward the taller eye.
  - **sheepish**: looks aside and down (10 px), two 2 px shuffles in the first
    second, a 300 ms glance back at 2 s; a 15 px sweat drop slides 6 px down
    the right eye's outer corner.
  - **relieved**: both eyes shut into the wink's arched line and reopen over
    700 ms, rising 4 px, then sink 6 px and hold.
  - **proud**: one 4 px chin-up nudge, the gaze straight up 8 px, two sparkles
    at the outer top corners (12 px arms) for about a second, twinkling again
    every 1.75 s until the time to live ends; one deliberate 300 ms blink at
    1.5 s.
  - **flattered**: looks down 8 px, a shy 3 px sway at 1 Hz, "///" blush
    strokes (20 px across, 3 px thick) on each outer, lower cheek.
  - Where these differ from `docs/expressions-design.json` (sheepish's gaze
    height, the mark sizes, relieved's smile, proud's twinkle, laughing's
    snap, sleepy's z timing), a contact sheet at phone scale showed the
    design's numbers did not read: sheepish looked like the since-dropped
    thinking, and the marks were specks.
  An expression that points the gaze owns it (the mood's glances wait). Marks
  are drawn in the face's own white and grow in and out rather than fade;
  when the expression ends they shrink away. Each id's motion runs from when it
  first showed, and does not replay if the owner talks and it shows again.
- Every shape eases in and out on critically damped springs (about a third of
  a second). That includes the scripted closures (relieved's exhale, proud's
  blink): an expression cut off while the eyes are shut reopens them over a
  few frames, not in one. With **Reduce Motion** it changes at once, with no
  animation.
- **VoiceOver** reads the face as its mood and the expression, for example
  "speaking, happy".

| Label | Left × right height | Width | Corner | Lid | Blink (s) | Curious |
|---|---|---|---|---|---|---|
| calm | 96 × 96 | 96 | 22 | none | 3 | no |
| happy | 86 × 86 | 96 | 22 | smile | 3 | no |
| curious | 100 × 88 | 96 | 22 | none | 3 | yes |
| affection | 76 × 76 | 100 | 32 | smile | 5 | yes |
| surprised | 110 × 110 | 84 | 36 | none | 5 | no |
| sad | 72 × 72 | 92 | 22 | droop | 5 | no |
| worried | 102 × 94 | 90 | 26 | droop | 2 | yes |
| skeptical | 58 × 96 | 94 | 18 | none | 4 | yes |
| frustrated | 70 × 70 | 94 | 16 | brow | 2 | no |
| excited | 110 × 110 | 104 | 26 | smile | 1 | no |
| wink | 96 × 96 | 96 | 22 | none (the wink) | 4 | no |
| laughing | 94 × 94 | 100 | 22 | smile 1.4 | 3 (none) | no |
| sleepy | 40 × 40 | 96 | 18 | flat | 6 (400 ms) | no |
| confused | 100 × 88 | 92 | 22 | none | 3 | no |
| sheepish | 78 × 78 | 92 | 22 | flat | 2 | no |
| relieved | 86 × 86 | 98 | 26 | droop + smile | 4 | no |
| proud | 80 × 80 | 100 | 24 | smile 0.5 | 3 | no |
| flattered | 66 × 66 | 100 | 28 | smile 0.8 | 4 | no |

The seven new rows are the phone's own until the firmware's `expression.h`
gains them; `Tests/run.sh` checks the board's eleven against that file.

Not verified yet: the shapes on the owner's iPhone in a real call (gate E6 in
`docs/phone-eyes-gates.md`).

## Files

| File | What it does |
|---|---|
| `ios/EveLink/App/VoiceCall.swift` | The host: starts and ends the hands-free call, owns the meter, sets the status line, the face's tap and hold (`TalkGesture`, moved from the Talk sheet), Settings and the voice settings sheet. |
| `ios/EveLink/App/VoiceModeView.swift` | The screen. It shows the levels and reports taps through closures; it does not own the call. |
| `ios/EveLink/App/StackChanFace.swift` | The face (`StackChanFace`, drawn with `Canvas` once per frame in a `TimelineView(.animation)`), its motion (`FaceMotion`: blinks, glances and openness on critically damped springs), eve's expression drawn over it (per-eye height, width, corner, RoboEyes' lids, the level lid, the wink, each label's motion and white marks) and the caption view. |
| `ios/EveLink/App/VoiceLevels.swift` | The level meter, the captions and the expression: PCM in, `level`, `speaker`, `caption` and `expression` out. |
| `ios/EveLink/Shared/FaceExpression.swift` | `FaceMood`, the eighteen labels and their shapes (the eleven from `expression.h`), each label's motion and marks (`pose(at:)`), the message's decoding, `FaceExpressionState` (newer ids, ttl, when a shape may show) and `FaceWink`: pure, checked on the Mac. |
| `ios/EveLink/Shared/PhoneTurn.swift` | `SpeechOnset`, `HandsFree`, `VoiceStatus`: the pure logic, checked on the Mac. |
| `ios/EveLink/Shared/Relay.swift` | `beginHandsFree`, `endHandsFree`, `muted`, `connected`, `callError`, `voiceLevels`, `handsFreeBackgrounded`, and the wait for queued playback before the mic reopens. |
| `ios/EveLink/App/PhoneAudio.swift` | `earpiece`: the session's category without `.defaultToSpeaker`. |

The orb (`VoiceOrb.swift` and its Metal shader) was removed with the face's arrival on 2026-10-09; the app no
longer needs Xcode's Metal Toolchain.

## How the level is measured

`VoiceLevels` takes the same 24 kHz mono 16-bit PCM that the call already moves.

1. RMS of each chunk, as a fraction of full scale.
2. RMS to decibels, then mapped linearly from the noise floor (−50 dBFS, level 0)
   to the ceiling (−12 dBFS, level 1). `VoiceLevels.level(fromRMS:)` is this
   step, as a pure function.
3. Who is speaking: the owner, if a chunk above level 0.06 came in the last
   300 ms (so the owner talking over eve wins); else eve, while his audio is
   still playing; else idle.

eve's audio arrives faster than it plays. So `evePCM` cuts each chunk into
50 ms windows and queues their levels on a playback clock. The face then bounces
with the word that is being heard, not with the network. `eveFlushed()` drops
the queue when eve is interrupted.

While anything is live, the meter ticks about 30 times a second, so silence
decays to idle with no new PCM. The face smooths its gaze, openness and width
with critically damped springs, so nothing jumps between frames.

## Wiring

`Relay.voiceLevels` (a `VoiceLevelSink`, which `VoiceLevels` is) is set while
voice mode is open:

- `fromPhoneMic`, after the mute check: `ownerPCM`.
- eve's audio, when it plays on the phone: `evePCM`.
- eve's `flush`, and the start of each phone turn: `eveFlushed`.
- the end of the call: `reset`.
- eve's `heard` (the owner's words): `ownerSaid`; his `said` (a sentence, before its sound): `eveSaid`.
- eve's `expression`: `showExpression`. The face reads `expression` through `FaceExpressionState.gate`.

The earpiece choice changes the audio session's category. If it is changed
mid-call, iOS reports a configuration change and `PhoneAudio.reconfigure`
restarts the engine, as when AirPods connect. This is not verified on a device
yet.

## Checks

The pure math (the dB scale, RMS, the 50 ms windows, the speaker rule, the
spring, the decay to idle and eve's playback clock) was checked on the Mac
with a scratch harness against `App/VoiceLevels.swift`. All 22 checks passed.
These checks are not in `Tests/run.sh` yet: that script compiles a fixed list of
files, and `VoiceLevels.swift` is not on it.

`Tests/run.sh` checks the expressions (`EXPRESSIONS_OK`): decoding good, bad
and clear messages; each label's shape against the numbers it reads from
`expression.h` itself; newer ids only, across the wrap; the ttl clamp and
expiry; the moods that may show one; and the wink's 650 ms once per id. The
captions run (`CAPTIONS_OK`) checks that `VoiceLevels` shows an expression,
refuses an older id, expires it on its own clock and clears it at the call's end;
and, with eve's audio queued ahead, that a pick for the next sentence waits for
that sentence's sound, runs its ttl from there, and never shows if eve is
flushed first. The face run (`FACE_OK`) builds the real `App/StackChanFace.swift`
on the Mac and checks that `FaceMotion` settles each expression at its table
height whatever the mood (surprised while thinking at 110/96, not 0.89). For the
seven new labels it checks the design's numbers; that each pose is a function of
the time, bounded, free of jumps, and still (or looping) after its settle time;
that Reduce Motion holds one pose with its marks still; that each mark belongs to
its label only; each motion against the design's words; and that the board's
eleven are unchanged; and that relieved or proud cut off while shut reopens over
frames. `sh ios/EveLink/Tests/Face/render.sh` draws a contact sheet
of all eighteen at 0.3, 2 and 3 s (3 s shows the settled poses) with the app's
own drawing code, to `ios/EveLink/build/expressions.png`. `--phone` draws it at
true phone scale (one pixel per point, each face in a 393 pt column), and
`--only happy,proud` draws just those labels.

`Tests/run.sh` also checks the hands-free logic (`VOICE_MODE_OK`): hiss never starts
a turn, 120 ms of voice is not enough, 150 ms of voice starts it with the 0.5 s
before it, a 5 ms full-scale click inside a 100 ms chunk is not speech, frames
carry across chunk edges, when the microphone opens again and when it must not, the status
line, and the `setInsets` numbers.

Not verified yet: 60 fps on an iPhone 17 Pro, the look on the device, a real
hands-free call (the onset threshold in a noisy room, echo while eve
speaks, the 0.25 s playback margin before the microphone reopens), the
background hang-up, recovery after an interruption, and the earpiece switch. The
face was checked in the simulator (iPhone 17 Pro Max, 2026-10-09); its captions
need a real call.

## Eyes on the owner's turn, and on-device expressions (2026-10-13)

- While the owner holds the button the face listens with still eyes; it reacts once their words come back from
  speech-to-text (owner: "eyes should start reacting when the stt happens not before"). The Mac already offered the
  owner's words to the expression picker only after transcription; the phone's eyes had been bobbing with the mic level.
- `EVE_EXPRESSION_JEV_ROUTE=ollama` points the expression picker (not the other Jev routers) at an open decision model
  on this Mac through Ollama 0.35+'s `/v1/systemone` (no key; `EVE_EXPRESSION_JEV_MODEL`, default `tev1:0.8b`). Off by
  default. Measured on the 84 legacy eye cases (`tools/jev_eyes_eval.py --legacy`): Jev over the network 62.5% / 69.4%
  at ~250 ms p50; tev1 0.8B on this Mac 52.1% / 50.0% at ~15 ms; tev1 4B 58.3% / 63.9% at ~470 ms. Neither reaches the
  bar (57.5 / 67.2), so Jev stays the default until a fine-tuned tev1 does.

