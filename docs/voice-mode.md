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
- **VoiceOver**: every control has a label. The face reads as "eve" with
  its mood ("listening", "thinking"…), and each caption is read as "You: …" or
  "eve: …".
- **Reduce Motion**: the eyes do not blink, glance or bounce. Only their
  openness follows the call.

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

## Files

| File | What it does |
|---|---|
| `ios/EveLink/App/VoiceCall.swift` | The host: starts and ends the hands-free call, owns the meter, sets the status line, the face's tap and hold (`TalkGesture`, moved from the Talk sheet), Settings and the voice settings sheet. |
| `ios/EveLink/App/VoiceModeView.swift` | The screen. It shows the levels and reports taps through closures; it does not own the call. |
| `ios/EveLink/App/StackChanFace.swift` | The face (`StackChanFace`, drawn with `Canvas` once per frame in a `TimelineView(.animation)`), its motion (`FaceMotion`: blinks, glances and openness on critically damped springs) and the caption view. |
| `ios/EveLink/App/VoiceLevels.swift` | The level meter and the captions: PCM in, `level`, `speaker` and `caption` out. |
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

`Tests/run.sh` checks the hands-free logic (`VOICE_MODE_OK`): hiss never starts
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
