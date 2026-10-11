# The OBSBOT camera

eve controls the owner's OBSBOT Tiny 3 webcam from the command line and from Eve Link. Both go through
**OBSBOT Center**, OBSBOT's app for the Mac, and its OSC server on `udp://127.0.0.1:16284`.

| Where | What it is |
|---|---|
| `eve-bridge obsbot ...` (alias `eve-bridge cam`) | The CLI. eve's coding sessions and scripts use it. |
| Eve Link, **Apps**, **Camera** | The camera's live picture, hold-to-turn arrows, zoom, center, sleep and wake, tracking and the AI mode. |

The code is `bridge/src/eve_bridge/obsbot.py` (OSC, the camera, the CLI) and `obsbot_panel.py` (the app's panel
and picture). In the app it is `App/ObsbotView.swift` and `Shared/ObsbotControl.swift`.

## How it was found

The owner asked on 2026-10-11 to reverse-engineer OBSBOT Center with [REA](https://github.com/morluto/rea).
OBSBOT Center 2.0.15 is a Qt app. Its camera driver is `Frameworks/libdev.dylib`, OBSBOT's device SDK (C++
`Device::ai*` and `Device::camera*` methods over USB). The main binary wraps a liblo `OSCServer` around that
driver, with 76 addresses under `/OBSBOT/...` (REA's `inspect-macho`, then `strings` and `nm`). OBSBOT documents
this surface in an
[OSC command sheet](https://www.obsbot.com/explore/obsbot-center/osc). So eve uses OSC and does not re-implement
the USB protocol. OSC keeps working after firmware updates, and the owner can still use the app by hand.

The cost: OBSBOT Center must run with OSC on. eve starts it hidden (`open -g`) when it is not running.

## Set up

1. Install OBSBOT Center and connect the camera.
2. Turn on OSC:

   ```bash
   eve-bridge obsbot setup
   ```

   This sets `OSC=true`, UDP, host `127.0.0.1` and port `16284` in
   `~/Library/Application Support/OBSBOT_Center/global.ini`. The app writes that file when it quits, so `setup`
   quits a running app first and starts it again. You can also turn on OSC in the app's settings.
3. For the picture in Eve Link, allow eve to use the camera. The first time the picture opens, macOS asks whether
   `python3.13` (the daemon) may use the camera. Allow it. If you miss the prompt, enable it in **System Settings,
   Privacy & Security, Camera**.

Without the camera permission, the controls still work. The picture shows one line that says what is missing.

The Camera row appears in Eve Link while OBSBOT Center is installed. `EVE_OBSBOT=0` in the daemon's environment
turns it off.

## The CLI

```text
eve-bridge obsbot status                     Camera, awake or asleep, where it points, zoom, presets
eve-bridge obsbot wake | sleep
eve-bridge obsbot move --pan 30 --pitch -10  Absolute: pan -129..129° (+ is left), pitch -59..59° (+ is down)
eve-bridge obsbot nudge left [--seconds 0.5] [--speed 50]
eve-bridge obsbot stop                       Stop a turn
eve-bridge obsbot center                     Straight ahead (pan 0, pitch 0)
eve-bridge obsbot zoom 0..100
eve-bridge obsbot fov 86 | 78 | 65
eve-bridge obsbot preset 1..3                A preset saved in OBSBOT Center
eve-bridge obsbot track on | off             AI tracking lock
eve-bridge obsbot mode single | group | voice | desk | hand | whiteboard
eve-bridge obsbot framing headroom | standard | motion
eve-bridge obsbot track-speed slow | standard | fast
eve-bridge obsbot focus auto | 0..100
eve-bridge obsbot mirror on | off
eve-bridge obsbot raw /OBSBOT/... [args]     Any OSC address; prints every reply for 0.7 s
eve-bridge obsbot setup                      Turn on OSC in OBSBOT Center
```

`--json` prints the result as JSON. `--device N` picks a camera when there are several. `--no-launch` does not
start OBSBOT Center. These options can come before or after the subcommand. The exit code is 0 when the camera did
what was asked.

`move`, `center` and `zoom` read the camera back. The reply says where the camera ended up, not where it was sent.

## The app

**Apps, Camera** opens one screen:

- **The picture** at the top. It is the webcam through the Mac, about 14 frames a second at 640 px. It shows only
  while a screen is open.
- **Arrows around a Center key.** Hold an arrow to turn the camera. Lift your finger to stop it. Left turns the
  view left, as the picture shows.
- **Zoom**, a slider. The zoom changes when you let go of the slider.
- **Sleep or Wake**, **Track** (the AI lock) and **Mode** (the AI modes).
- **Presets**: the presets saved in OBSBOT Center, shown only when there are some.

The line under the picture shows where the camera points. When something goes wrong, it shows what went wrong.

When **Track** is on, the camera follows a person. It does not stay where the arrows put it. Turn Track off to aim
the camera by hand.

### The picture

The app asks for the picture over WebRTC first. On the home Wi-Fi, the picture then comes straight from the Mac to
the phone. Away from home, it goes through each side's public address (STUN). It uses Pika's wire format
(`pika_rtc.py`): one data channel, and each message is a stamp followed by one JPEG. When WebRTC cannot connect,
the app uses the stream through the tunnel instead.

Measured on 2026-10-11 from the Mac:

| Path | Frames | Gap between frames |
|---|---|---|
| WebRTC | about 14 a second | p50 67 ms, p95 77 ms, max 109 ms |
| The tunnel | about 9 a second | many arrive together, then gaps up to 0.7 s |

One ffmpeg serves every viewer, so two open screens (a phone and an iPad, say) both get the picture. ffmpeg stops 3
seconds after the last screen closes.

Before this design, the camera served one viewer, and a new viewer ended the old one. On 2026-10-11 a phone and a
simulator both had the screen open. Each took the camera from the other every 3 seconds, and the picture kept
stopping. This is why viewers now share one capture.

### Safety

Holding an arrow works like [Pika's](pika.md#safety) hold-to-drive. The camera only turns while you hold:

- While you hold an arrow, the app sends a beat about every 150 ms. Every beat sends the turn again, so one lost
  UDP packet does not stop a held arrow.
- The Mac stops the gimbal when you release the arrow, when no beat came for 450 ms, when the screen closes, and
  when the daemon stops.
- Each press has its own id. If a beat arrives late, after its press ended, the Mac refuses it. So a late beat never
  starts the camera turning again.

## What was measured (Tiny 3, OBSBOT Center 2.0.15, 2026-10-11)

These facts differ from OBSBOT's sheet, or the sheet does not mention them:

- **The position reply has two numbers.** `GetGimbalPosInfoResp` is `[pan, pitch]`, not the sheet's three. Positive
  pitch tilts down. Asleep, the lens parks at pitch +79.
- **A query takes the device index alone.** `GetDeviceInfo 0 0`, with the sheet's reserved second value, gets no
  answer. `ResetGimbal` behaves the same way. Even with the index alone, the reset was ignored one time in three.
  So `center` is a move to (0, 0).
- **Use the integer move command.** `SetGimMotorDegreeEx` (floats) moved pan but not pitch. `SetGimMotorDegree`
  (integers) moves both.
- **Speed 30 is the reliable move speed.** At speed 60, every move smaller than about 20° was dropped. At 90, small
  moves overshot. At 40, two of six missed. At 30, every move landed within 2°, at about 25° a second. eve sends 30,
  and when a move stops short, it sends the move once more at speed 20.
- **The last degree of a move creeps in for up to about 2 s.** A move reports done only after the position stays
  the same for about 0.75 s.
- **`SetZoomSpeed` does nothing on the Tiny 3.** `SetZoom` works, and takes about 2 s from end to end.
- **Wake from a resting camera takes about 0.6 s.** A wake sent while the camera is still parking after a sleep is
  ignored, so eve sends it again once.
- **`AiTrackingInfo` is the target lock, not whether tracking is on.** So a tracking change is sent but not claimed
  as confirmed.
- **The camera's picture and OBSBOT Center can run at the same time.** ffmpeg reads the camera through AVFoundation
  while the app has it open. The first frame takes 0.45 s, and a frame is about 18 KB at 640 px.

### A crash in OBSBOT Center

OBSBOT Center 2.0.15 once crashed on an OSC sleep (SIGSEGV). The crash was in
`OSCServer::RefreshClientsStateInfo`, called from the app's sleep handler: the function's second instruction reads
the OSC server's liblo handle, and that handle was null. eve cannot fix this crash inside the app. eve does two
things:

- After a `sleep` or `wake`, eve checks that the app is still running. When the app crashed, eve starts it again
  and says so in its reply.
- Every eve client sends the sheet's optional `Disconnected` when it closes. OBSBOT Center keeps every sender as a
  client and refreshes each one on every state change. Before this change, eve's test runs had left dozens of
  dead clients.

After this change, five sleep and wake cycles ran with no crash. The cause is not proven.

## Security

OBSBOT Center binds port 16284 on all interfaces. So any device on the LAN can send it commands while OSC is on.
OBSBOT's app makes this choice, and eve cannot change it. The picture and the panel in Eve Link need the link token,
like every Eve Link control.
