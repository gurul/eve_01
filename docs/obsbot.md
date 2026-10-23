# The OBSBOT camera

eve controls the owner's OBSBOT Tiny 3 webcam. You can ask eve in the chat, in Eve Link's chat or out loud; you can
use the **Camera** screen in Eve Link; and scripts can use the command line.

| Where | What it is |
|---|---|
| Ask eve ("camera left", "point the camera at the door", "zoom in on me") | The chat and voice brains' camera tools. See [Asking eve](#asking-eve). |
| Eve Link, **Apps**, **Camera** | The camera's live picture, hold-to-turn arrows, zoom, center, sleep and wake, tracking and the AI mode. |
| `eve-bridge obsbot ...` (alias `eve-bridge cam`) | The CLI. eve's coding sessions and scripts use it. |

eve talks to the camera two ways:

- **Directly over USB** for everything that moves it: pan and tilt, a continuous turn, zoom, centering and waking.
  This needs no other app. See [Direct USB](#direct-usb).
- **Through OBSBOT Center**, OBSBOT's app for the Mac, for OBSBOT's own features: AI tracking, the AI modes,
  presets, sleep, and the picture settings (field of view, focus, mirror). eve uses its OSC server on
  `udp://127.0.0.1:16284`, and only while the app is running. eve never starts the app.

The code is in `bridge/src/eve_bridge/`: `obsbot_usb.py` and `native/obsbot_uvc.c` (USB), `obsbot.py` (the
camera, OSC and the CLI) and `obsbot_panel.py` (the models' tools, the app's panel and the picture). In the app it is
`App/ObsbotView.swift` and `Shared/ObsbotControl.swift`.

## How it was found

The owner asked on 2026-10-11 to reverse-engineer OBSBOT Center with [REA](https://github.com/morluto/rea).
OBSBOT Center 2.0.15 is a Qt app. Its camera driver is `Frameworks/libdev.dylib`, OBSBOT's device SDK (C++
`Device::ai*` and `Device::camera*` methods over USB). The main binary wraps a liblo `OSCServer` around that
driver, with 76 addresses under `/OBSBOT/...` (REA's `inspect-macho`, then `strings` and `nm`). OBSBOT documents
this surface in an [OSC command sheet](https://www.obsbot.com/explore/obsbot-center/osc).

eve first used only OSC. The same day, the app crashed on an OSC sleep, left an orphaned helper process holding its
port, showed "OBSBOT Center is already running", and turned its own OSC setting off. The owner asked: "can't u just
interface with [the camera] directly". The camera's movement turned out to be standard USB video (UVC) controls,
which need no OBSBOT protocol. So movement moved to USB, and OSC is kept only for what UVC does not have.

## Set up

1. Connect the camera. For moving it, that is all eve needs, plus Apple's command line tools (`clang`). eve builds
   its small USB helper the first time it needs it.
2. Check it:

   ```bash
   eve-bridge obsbot setup
   ```

   This builds the helper and reads the camera over USB. When OBSBOT Center is installed and **not running**, it
   also turns on OSC in `~/Library/Application Support/OBSBOT_Center/global.ini` (`OSC=true`, UDP, host
   `127.0.0.1`, port `16284`), for the AI features. A running app is left alone: it rewrites that file when it
   quits, and asking it to quit shows a dialog. Turn on OSC in the app's settings instead.
3. For the picture in Eve Link, allow eve to use the camera. The first time the picture opens, macOS asks whether
   `python3.13` (the daemon) may use the camera. Allow it. If you miss the prompt, enable it in **System Settings,
   Privacy & Security, Camera**. Waking the camera uses the same permission.

The camera's tools and the Camera row are on while an OBSBOT camera is plugged in (or OBSBOT Center is installed).
`EVE_OBSBOT=0` in the daemon's environment turns them off.

## Asking eve

A message that is **only** a camera command, and names the camera, is done by code with no model turn. It takes
about a second, and the reply says where the camera ended up ("Camera pointing 20° left.").

| You say | What happens |
|---|---|
| "camera left", "turn the camera right a bit", "pan the webcam left 30 degrees", "camera up a lot" | A turn: 20° with no amount, 10° for "a bit", 45° for "a lot" |
| "center the camera", "camera straight ahead" | Back to straight ahead |
| "zoom the camera in", "camera zoom out a bit", "camera zoom 50%" | Zoom by 25 (12 for "a bit"), or to a level |
| "wake the camera", "camera off", "put the camera to sleep" | Wake (USB), or sleep (needs OBSBOT Center) |
| "where is the camera pointing", "is my webcam awake" | Where it points, the zoom, awake or asleep |
| "camera track me", "camera stop tracking", "camera preset 2" | OBSBOT's own features (need OBSBOT Center) |

Everything else goes to the model, which has two tools: `camera_status` and `camera_control` (turn, point,
center, zoom, wake, sleep, tracking on or off, the AI mode, a preset). For example, "point the camera at the door",
"get me in frame", "zoom in on my face" and "follow me around" all go to the model. To aim at something by name,
the model first takes a photo with `take_photo` (the same camera) and then turns toward it. The voice has the same
two tools.

Measured on 2026-10-11 with the production chat model and routing, on a blind set of 30 messages written without
the code: every camera request (20 of 20) went to the camera, and every photo request (4 of 4) to `take_photo`. No
message that was not about the camera moved it.

## The CLI

```text
eve-bridge obsbot status                     Camera, awake or asleep, where it points, zoom, presets
eve-bridge obsbot wake | sleep
eve-bridge obsbot move --pan 30 --pitch -10  Absolute: pan -129..129° (+ is left), pitch -59..59° (+ is down)
eve-bridge obsbot nudge left [--seconds 0.5] [--speed 40]
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
eve-bridge obsbot setup                      Build the USB helper, read the camera, turn on OSC
```

`--json` prints the result as JSON. `--device N` picks a camera in OBSBOT Center when there are several. These
options can come before or after the subcommand. The exit code is 0 when the camera did
what was asked.

`move`, `center` and `zoom` read the camera back. The reply says where the camera ended up, not where it was sent.

`status`, `wake`, `move`, `nudge`, `stop`, `center` and `zoom` go over USB and work without OBSBOT Center. `sleep`,
`fov`, `preset`, `track`, `mode`, `framing`, `track-speed`, `focus`, `mirror` and `raw` need OBSBOT Center running
with OSC on; without it they answer in one line and exit 1.

## The app

**Apps, Camera** opens one screen:

- **The picture** at the top. It is the webcam through the Mac, about 14 frames a second at 640 px. It shows only
  while a screen is open.
- **Arrows around a Center key.** Hold an arrow to turn the camera. Lift your finger to stop it. Left turns the
  view left, as the picture shows.
- **Zoom**, a slider. The zoom changes when you let go of the slider.
- **Sleep or Wake**, **Track** (the AI lock) and **Mode** (the AI modes). Sleep, Track and Mode need OBSBOT Center
  running; without it the line under the picture says so.
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

- While you hold an arrow, the app sends a beat about every 150 ms. The first beat wakes the camera if it is asleep
  and starts its continuous USB turn; the rest only renew the press.
- The Mac stops the gimbal when you release the arrow, when no beat came for 450 ms, when the screen closes, and
  when the daemon stops.
- Each press has its own id. If a beat arrives late, after its press ended, the Mac refuses it. So a late beat never
  starts the camera turning again. A release while the first beat is still waking the camera (up to about
  7 s) holds too: the turn never goes out after the wake.

## Direct USB

`native/obsbot_uvc.c` is a small C program that eve compiles with `clang` the first time it needs it, into
`~/.config/eve-bridge/bin/obsbot-uvc-<hash>` (an edit to the source builds a new one). Each call is one process and
takes about 7 ms. It sends standard UVC camera-terminal requests (UVC 1.5, section 4.2.2.1) to the camera:

| Call | UVC control |
|---|---|
| `get` | pan/tilt absolute and zoom absolute: the current values and their ranges, and the camera's name |
| `pantilt PAN TILT` | pan/tilt absolute, in degrees |
| `turn PAN TILT [SPEED]` | pan/tilt relative: a continuous turn until `turn 0 0` |
| `zoom N` | zoom absolute |

The Mac's own webcam driver owns the camera, so a libusb request is refused ("access denied"). Apple's IOKit still
sends a control request to the camera's default pipe without claiming the device, and the Tiny 3 answers it.

Measured on a Tiny 3, 2026-10-11:

- **Ranges:** pan ±130°, tilt ±90°, zoom 0 to 100. Positive pan turns the view left, positive tilt turns it up.
  Moves land within about 1°, small ones too.
- **The continuous turn's "+1" pans right** (the absolute pan goes down). `obsbot_usb.turn` maps the directions, so
  callers never see this. At the camera's top speed (80) it turns about 32° a second; eve uses 40.
- **Asleep, the camera accepts commands and ignores them.** It reads tilt -79 (the lens parked down). Opening its
  video for about a second wakes it, and the gimbal unparks. After the video closed, it stayed awake for at least a
  minute. eve wakes it this way before any move. Moves keep tilt within ±59°, so "asleep" and "pointed down" never
  look the same.
- **The camera ignores an absolute move to the target it last took**, and a continuous turn moves the gimbal
  without changing that target (measured 2026-10-23). So after a hold-to-turn, "center" sent (0, 0) again and the
  camera stayed put: 13 times in 14. A resend, or a point at where it already is, was ignored too. `Obsbot.move`
  first sends a real move a few degrees off (halfway on a long move, 3° back on a short one), then the target: 14
  of 14 landed first time. A move that still stops short is sent again from there, up to three times.
- **OBSBOT's own features are a vendor extension unit**, with 32 command slots of 60-byte packets. eve does not
  speak that protocol: tracking, the AI modes, presets and sleep go through OBSBOT Center.

## What was measured over OSC (Tiny 3, OBSBOT Center 2.0.15, 2026-10-11)

eve's first version moved the camera over OSC. Movement now goes over USB, but these facts still apply to OBSBOT
Center's OSC, and some explain why movement left it. They differ from OBSBOT's sheet, or the sheet does not mention
them:

- **The position reply has two numbers.** `GetGimbalPosInfoResp` is `[pan, pitch]`, not the sheet's three. Positive
  pitch tilts down. Asleep, the lens parks at pitch +79.
- **A query takes the device index alone.** `GetDeviceInfo 0 0`, with the sheet's reserved second value, gets no
  answer. `ResetGimbal` behaves the same way. Even with the index alone, the reset was ignored one time in three.
  So `center` is a move to (0, 0).
- **Use the integer move command.** `SetGimMotorDegreeEx` (floats) moved pan but not pitch. `SetGimMotorDegree`
  (integers) moves both.
- **OSC moves depend on their speed argument.** At speed 60, every move smaller than about 20° was dropped. At 90,
  small moves overshot. At 40, two of six missed. At 30, every move landed within 2°.
- **The last degree of a move creeps in for up to about 2 s.** A move reports done only after the position stays
  the same for about 0.75 s.
- **`SetZoomSpeed` does nothing on the Tiny 3.** `SetZoom` works, and takes about 2 s from end to end.
- **A sleep or wake sent while the camera is still moving to or from its parked position is ignored**, so eve sends
  sleep again once.
- **`AiTrackingInfo` is the target lock, not whether tracking is on.** So a tracking change is sent but not claimed
  as confirmed.
- **The camera's picture and OBSBOT Center can run at the same time.** ffmpeg reads the camera through AVFoundation
  while the app has it open. The first frame takes 0.45 s, and a frame is about 18 KB at 640 px.

### A crash in OBSBOT Center

OBSBOT Center 2.0.15 once crashed on an OSC sleep (SIGSEGV). The crash was in
`OSCServer::RefreshClientsStateInfo`, called from the app's sleep handler: the function's second instruction reads
the OSC server's liblo handle, and that handle was null. eve cannot fix this crash inside the app. eve does two
things:

- After an OSC `sleep`, eve checks that the app is still running, and says so in its reply when it crashed.
- Every eve client sends the sheet's optional `Disconnected` when it closes. OBSBOT Center keeps every sender as a
  client and refreshes each one on every state change. Before this change, eve's test runs had left dozens of
  dead clients.

After this change, five sleep and wake cycles ran with no crash. The cause is not proven.

Two more things about the app, found while restarting it:

- When `OBSBOT_Main` dies (a crash, or a kill), its helper process (`Contents/Modules/OBSBOT_Center.app`) lives
  on, holding UDP 16284. A new OBSBOT Center then shows "OBSBOT Center is already running" and never opens OSC.
  Quit the helper too (Activity Monitor, or `pkill -f 'Modules/OBSBOT_Center.app'`) before starting the app.
- After those forced restarts, the app had turned its own `OSC` setting off. Run `eve-bridge obsbot setup` with the
  app closed, or turn OSC on in its settings.

This is why eve no longer starts OBSBOT Center itself.

## Security

OBSBOT Center binds port 16284 on all interfaces. So any device on the LAN can send it commands while OSC is on.
OBSBOT's app makes this choice, and eve cannot change it. If you don't use tracking or the AI modes from eve, you can
leave OSC off: movement goes over USB. The picture and the panel in Eve Link need the link token, like every Eve
Link control. The USB helper runs only on this Mac and reaches only an OBSBOT camera.
