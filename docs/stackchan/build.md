---
title: StackChan build
icon: 🤖
order: 1
---

# StackChan build (M5StackChan K151)

`firmware/claude_pet_stackchan` is the eve port for the **M5StackChan AI
Desktop Robot** (SKU K151, CoreS3 core). Same NDJSON protocol, same
`eve-bridge` daemon. The robot mirrors Claude Code's state with its head,
eyes, 12 LEDs, and R2D2-style chirps. With the Mac attached it also turns
toward you, learns your face, and looks around the room when Claude is idle.

See also [capabilities.md](capabilities.md) (hardware, bench facts),
[repos.md](repos.md) (vendored repos), [widget.md](widget.md) (desktop widget),
and `bridge/README.md` (Listen key, Host vision, Owner identity, Idle explorer).

## Hardware

| Part | Detail |
|---|---|
| Core | M5Stack CoreS3: ESP32-S3, 16 MB flash, QSPI PSRAM, native USB-Serial/JTAG (`/dev/cu.usbmodem*`, VID 0x303a PID 0x1001) |
| Screen | ILI9342C 320x240, used in landscape. FT6336U touch |
| Camera | GC0308, QVGA RGB565, big-endian per pixel (bench 2026-09-05) |
| Head | 2x Feetech SCS0009 serial servos on UART1 (G6/G7, 1 Mbaud). Yaw id 1, pitch id 2 |
| LEDs | 12x WS2812C on the top board, written as RGB565 through the PY32L020 expander (I2C 0x6F) |
| Top touch | Si12T three zones: front, middle, back |
| Power | 550 mAh body battery through the AXP2101 PMIC. INA226 monitor. Servo 5 V rail gated by the expander (`VM_EN`) |

Two USB-C ports carry data. M5Stack recommends the base port. Opening the CDC
port does **not** reset the sketch, so the daemon can hold it open.

## Toolchain

| Item | Version | Where |
|---|---|---|
| arduino-cli + esp32 core | 3.3.10 | `arduino-cli core install esp32:esp32` |
| FQBN | `esp32:esp32:m5stack_cores3:PartitionScheme=huge_app,PSRAM=enabled` | `tools/flash_stackchan.sh` |
| M5Unified | 0.2.21 | Library Manager |
| M5GFX | 0.2.28 | Library Manager |
| StackChan-BSP | 1.1.0 **at main, commit `8d4d6fc` (2026-08-28)** — not the `1.1.0` tag, which predates `TouchSensor.recalibrate()` and does not compile this firmware | symlink `~/Documents/Arduino/libraries/M5StackChan` → `vendor/stackchan/StackChan-BSP` |
| FluxGarage RoboEyes | 1.1.2 | symlink `~/Documents/Arduino/libraries/FluxGarage_RoboEyes` → `vendor/stackchan/RoboEyes` |
| IRremoteESP8266 2.9.0, M5Unit-NFC 0.1.1 | — | BSP dependencies, Library Manager |
| AnimatedGIF | 2.2.0 | Library Manager (`character.cpp`); was missing from this table until a clean machine needed it, 2026-09-21 |

Re-clone the vendored repos with the loop in [repos.md](repos.md#re-clone).

## Flash

```bash
./tools/flash_stackchan.sh                        # compile → archive ELF → boot daemon out → upload → daemon back
./tools/flash_stackchan.sh /dev/cu.usbmodem3101   # explicit port
```

The script archives the exact ELF under `firmware/build-archive/`, boots the
launchd daemon out (it holds the port with `KeepAlive`), waits for the file
descriptor to close, uploads, and bootstraps the daemon back.

**Backup before the first flash.** The factory image is on
`firmware/build-archive/stackchan-factory-20260905.bin` (full 16 MB dump,
gitignored). Restore it, NVS included (WiFi, app pairing, servo zero offsets):

```bash
esptool -p /dev/cu.usbmodem* write-flash 0x0 firmware/build-archive/stackchan-factory-20260905.bin
```

Do not pass `-b`/`--baud` on this link: a baud switch stalls the
USB-Serial/JTAG transfer (bench: `-b 460800` died at 110 KB). Download mode:
hold RST about 3 s until the green LED next to it lights, then release.

## The face

RoboEyes (FluxGarage) on a 320x204 1-bit canvas pushed into the frame every
tick: two 96x96 eyes, radius 22, 36 px apart. The eye colour is a random HSV
hue per boot (printed as `[eyes] colour #RRGGBB`; `EYE_COLOUR_OVERRIDE` in
`src/eyes.h` fixes it). One status word sits under the eyes in the same
colour. No clock. The ASCII species and GIF renderer compile but are not
drawn. A caption page owns y >= 112, so the eyes park on the top row.

## States

`derive()` in `src/main.cpp` maps the heartbeat to a persona state. No daemon
connected → IDLE (awake, looking around).

| State | Trigger | Head | Eyes | LEDs | Chirp |
|---|---|---|---|---|---|
| SLEEP | connected, nothing running | pitch 10, torque releases at rest | TIRED, closed, a 600 ms peek every ~20 s | very dim blue breathe (4 s) | descending, quiet |
| BUSY | any session running | level (pitch 45), nod every 2.5 s | squint (67 px), curiosity | steady dim cyan | wake whistle when leaving sleep |
| ATTENTION | a session waits on a permission | pitch 70, then the gaze policy | ANGRY, horizontal flicker; sweat on a destructive prompt | orange pulse, 800 ms, all 12 | phrase + 4..6 beeps, again every 30 s |
| CELEBRATE | session completed | quick yaw shake, 0.6 s | HAPPY, laugh | green | trill |
| HEART | head pat (front zone) | tilt toward the toucher, pitch +8 | HAPPY, curiosity | pink | trill |
| DIZZY | (kept from the pet build) | 1.2 s yaw wobble | flicker, confused | off | wobble |
| Listening | Option held on the Mac | toward a toucher seen in the last 30 s, else centre; pitch 60 | wide (110 px) | blue (mic live) | one short up-chirp |
| Explore | `{"cmd":"mode","explore":true}` | host `look`s when held; otherwise looks around on its own — amplitude, tempo and a pitch bias follow the affect engine (`mood.cpp`) | openness, smile/droop, curiosity, blink and saccade tempo from the feeling | the feeling's colour and pulse (0.25 / 0.5 / 2.5 Hz) | one chirp per feeling change |
| Agent phases | `{"cmd":"agent","state":..}` | listening: faces you · thinking: tilted, slow side-to-side · speaking: bobs · working: down at the desk, typing glances · asking: up · done: nod · error: wobble | per phase ([personality.md](personality.md) § 2) | animations over the two back rows: blue wave · cyan scanner · white sparkle · cyan ripple · orange alternating · green sweep · red flash | wake / talk babble / "hm?" / beep-boop / boop |

Head motion: the BSP runs a spring per servo. On top of it `body.cpp` glides
each target along `easeInOutCubic` and streams the pose at 25 Hz
(auto-angle-sync off, servo speed 100..600 scaled to the step; glide time
max(300 ms, 6 ms/deg)). There is no idle micro-drift: streaming tiny targets
forever kept the servos under torque and whining (owner report 2026-09-23), so
the head now comes to rest and the BSP's auto torque release lets it go. Pitch is clamped to
5..85, yaw to ±60. `PITCH_LEVEL` 45 / `PITCH_SLEEP` 10 / `PITCH_ATTENTION` 70
depend on the servo zero (NVS `servo/zero_pos_2`): bench-tune them.

**Rest pose.** Idle, busy (Claude sessions running) and asleep all rest chin-down at pitch 5, the owner's
choice on 2026-09-27 ("all the way down"); the working nod is a small lift from there, and the attention
pose is 60. Level (45) is only a reference now. Change these in `body.cpp` (`PITCH_REST`, `PITCH_SLEEP`).

**Axis directions and torque.** The head holds torque at rest (`setAutoTorqueReleaseEnabled(false)`):
released, it falls back up to the ceiling from the rest pose. Each axis can be mirrored by a saved setting
(NVS `body`: `p_rev`, `y_rev`, both off as built), set from the calibration page `eve-bridge head-cal`
([vision.md](vision.md#the-head-fell-back-up-and-the-calibration-page-2026-09-27)). Serial diagnostics (stop
the daemon first; it holds the port): `{"cmd":"axis"}` reads or sets the directions,
`{"cmd":"pitch_zero"}` reads the servo's stored zero (raw steps, BSP default 620; `"raw":N` stores a new one in
520..700 and reboots — leave it at 620 unless the neck was rebuilt), and `{"cmd":"ext_power","on":bool}`
toggles the CoreS3 external 5 V for one boot and acks with a fresh servo read.

Chirps (`src/chirp.cpp`) are synthesized to 8-bit PCM at 16 kHz into two 32 KB
PSRAM buffers and played asynchronously through `M5.Speaker.playRaw()`. The
CoreS3 amplifier is disabled during setup and between sounds to prevent idle
hiss/whine. `chirpUpdate()` allows 150 ms after the mixer goes idle for the I2S
DMA tail to drain, then calls `M5.Speaker.end()` (logged as
`[chirp] speaker off (idle)`). The next sound restarts the speaker. Muting
immediately stops playback and disables the amplifier; unmuting alone leaves
it off. Startup/shutdown can briefly wait for the driver, but the drain timer
does not delay the main loop. To verify the lifecycle without hardware:

```bash
c++ -std=c++17 -Wall -Wextra -Werror \
  -Ifirmware/claude_pet_stackchan/host/chirp_stubs \
  firmware/claude_pet_stackchan/host/chirp_test.cpp \
  firmware/claude_pet_stackchan/src/chirp.cpp -o /tmp/eve-chirp-test
/tmp/eve-chirp-test
```

After flashing, listen through a chirp and the following idle interval, and
check that mute stops an active chirp and unmute allows the next one. If noise
continues with the amplifier off, the source needs separate hardware diagnosis.

Quiet is the default as of the 2026-09-22 bench test: the owner confirmed that
turning off external 5 V stopped the continuous tone, and it returned when the
45-second test restored power. `halBegin()` now calls `M5.Power.setExtOutput(false)`
on every boot, with `[power] quiet default: external_5v=0` as readback.
The rear LEDs and top Si12T touch sensor share this supply and are unavailable;
top touch polling/calibration is skipped. The screen and the head motors have
separate power supplies. The owner chose silence over those auxiliary features.

For bench isolation, send `{"cmd":"noise_test","target":"screen"}` over
serial. Targets are `screen` (backlight off), `motors` (servo rail off),
`external` (external 5 V output off), and `speaker` (force the speaker driver
off). Only one target is active at a time. Switching targets first restores the
previous one; `restore` ends a test immediately, and every test expires after
45 seconds. The board acknowledges `noise_test` and logs `[noise] restored`.
Keep the robot on a stable surface: the head can relax with motor power off.
The screen brightness and external-output setting are restored; motor power
returns on, and the speaker stays in its normal idle-off state until a chirp.
These tests do not persist settings. Listen during each test and its restored
baseline before choosing a fix; an acknowledged command does not prove the
physical noise stopped. External output may power attached accessories.

When auxiliary power is enabled in a custom build, top touch calibration must
wait 3 s after boot: the Si12T baseline is stale while the servo rail comes up
(the middle zone read pressed at boot, a phantom hold). Quiet mode leaves it off.

## Controls

| Input | What it does |
|---|---|
| Tap the panel (attention only) | raise the blocked session's terminal |
| Swipe down | Enter on the Mac |
| Swipe left / right | previous / next option in Claude Code's pickers |
| Bottom-right strip | scroll back through the transcript |
| **Front zone** tap (< 450 ms) | attention: focus terminal. Otherwise HEART one-shot |
| **Back zone** | same as the bottom-right strip |

The permission swipe card and hold-to-talk are both gone (owner request). The
board decides nothing: every permission defers to Claude Code's own terminal
prompt, and a hold on the pet is read and discarded. A touch during a
conversation does nothing: it neither hushes it nor raises a terminal, because a
phantom pad blip killed a live task on 2026-09-10 and a terminal raise would steal
the front app from a running computer task. End a conversation by voice.

## Gaze policy

`src/gaze.cpp` decides where the head looks. Sources, highest priority first:

1. **Host face** (`{"cmd":"face"}`, conf >= 40). Absolute angles use the
   yaw/pitch echoed in the cmd (the pose at capture), never the current one.
   `who:"owner"` trains the owner memory; `who:"unknown"` gets one glance in
   BUSY/IDLE and never trains it. Host silent for 3 s → on-board fallback.
2. **On-board detector** (`src/look.cpp`, core 0 task): motion centroid plus a
   skin-colour blob at ~10 fps; frames near a head move are quarantined.
3. **Owner memory** (`src/owner_model.h`): a habit histogram of where the owner
   tends to be, saved to NVS namespace `owner` (every 5 min or when confidence
   crosses 60, at most once per minute). A touch counts as an observation.

Owner wanted (ATTENTION or listening): live target → remembered spot →
**search sweep**, two rows (pitch 45 then 65, yaw -40..40, ~4.3 s), repeated
every 5 s while no live target is younger than 5 s. BUSY/IDLE only glance:
8° deadband, at most every 3 s, 3 s hold. SLEEP and the one-shots observe but
never move. `{"cmd":"look"}` (rules in `src/hostlook.h`) is honoured in any state
during a voice conversation — the owner asked for it out loud — and in SLEEP/IDLE/BUSY
otherwise, never while the owner is wanted, never with a card up or while dictating. It
may turn the full neck range (±120°). While its hold lasts it owns the head: automatic
gaze waits, and the conversation's phase poses and beats stand aside, so eve keeps
looking where it was told while it answers (host test: `host/hostlook_test.cpp`).
`{"cmd":"owner","op":"reset"}` wipes the
memory. Geometry: camera HFOV assumed 66°, VFOV 3/4 of that; `kYawSign` +1
(bench: the head follows the hand); `kElevSign` is **unverified**.

## Wire additions

Every other verb is the pet build's (`time`, `status`, `focus`, `key`).

| Direction | Line | Purpose |
|---|---|---|
| host → board | `{"cmd":"listen","on":true\|false}` | Option key down/up on the Mac |
| host → board | `{"cmd":"cam","on":true,"fps":5,"w":160,"h":120}` / `{"cmd":"cam","on":false}` | start/stop the frame stream |
| board → host | `{"frame":{"seq":n,"w":160,"h":120,"fmt":"jpeg","b64":"...","yaw":Y,"pitch":P}}` | one line per frame, written from the camera task |
| host → board | `{"cmd":"face","seq":n,"bx":..,"by":..,"size":..,"conf":..,"yaw":Y,"pitch":P,"who":"owner"\|"unknown"}` | largest face; `conf:0` = no face |
| host → board | `{"cmd":"look","yaw":-120..120,"pitch":5..85,"hold":ms}` | absolute pose request (explorer, and the voice's `move_head` / `look_around` / `find`); `+yaw` = the robot's right; `hold:0` hands the head back |
| host → board | `{"cmd":"move","kind":"osc","yaw_amp":25,"pitch_amp":12,"period_ms":900,"phase_deg":90,"cycles":8,"dwell_pct":8,"jitter_pct":10}` | a rhythm the **board** runs by itself at 25 Hz — one line for a whole dance instead of one per beat. Every number is re-checked by `motion::admit()` on the firmware, which shrinks amplitude to fit the travel, the per-axis velocity budget and the bout cap. The beat is kept and the swing yields. Centres are optional: without them the rhythm grows out of the pose the head already holds |
| host → board | `{"cmd":"move","kind":"keys","keys":[[at_ms,yaw,pitch,speed],…]}` | up to 8 keyframes for a gesture whose shape is the point; `127` on an axis leaves it alone |
| host → board | `{"cmd":"move","kind":"stop"}` | stop the motion where it is, and hand the head back to gaze |
| board → host | `{"pose":{"y":<tenths>,"p":<tenths>}}` | the commanded pose in tenths of a degree, at 10 Hz **while a motion runs** and never otherwise. This exists because an echo saying a motion ran is not evidence it moved: the first dance reported `ran=7200ms` with a motionless head. One last line carries `"end":true` as the motion finishes, so a watcher knows the stream stopped rather than stalled |
| host → board | `{"cmd":"sound","on":true\|false}` | the owner muted / unmuted eve; persisted (NVS `s_snd`) and applied to every chirp and beep; motion and LEDs unaffected. The status ack reports it as `"snd"` |
| host → board | `{"cmd":"led","on":true\|false}` | back LEDs on / off; persisted (NVS `s_led`) and applied every loop. Acked as `led` |
| host → board | `{"cmd":"mode","explore":true\|false}` | enter/leave explore mode |
| host → board | `{"cmd":"snap"}` | one full-size photo: the look task answers with a single frame line at 320x240, quality 85, carrying `"snap":true` |
| host → board | `{"cmd":"owner","op":"reset"}` | forget the owner memory |
| host → board | `{"cmd":"agent","state":"wake"\|"listening"\|"thinking"\|"speaking"\|"working"\|"asking"\|"done"\|"error"\|"idle"}` | the voice / computer-control conversation's phase; the robot acts it out ([personality.md](personality.md) § 2) |
| host → board | `{"cmd":"emote","dv":-100..100,"da":-100..100,"label":"curious"}` | the diary's appraisal of what the camera saw; nudges the affect engine by at most ±0.3 |
| host → board | `{"cmd":"caption","page":i,"of":n\|0,"lines":["..."],"hold_ms":ms,"chirp":bool,"final":bool}` / `{"cmd":"caption","clear":true}` | one page of eve's reply, pre-wrapped by the host (≤ 4 lines × 17 chars, size-3 text at y 112); shown for `hold_ms` (+3 s grace), one talk chirp when `chirp` is true; `clear` removes it |

Frames pause while a character transfer owns the wire. On this machine: ~4 fps,
~2.5 KB JPEGs (quality 60), macOS Vision detects faces in 5–20 ms per frame.

## Named motion

eve used to move one absolute pose at a time, so a rhythm cost a model round
trip per beat. Measured: 2.3 s median from the words to the head moving, of which
the delegated model call was 1.86 s. The vocabulary therefore lives on the board
(`src/motion.h`, `src/body.cpp`), and the host only names a preset.

```bash
eve-bridge move dance            # a preset
eve-bridge move sway --speed 1.6 # a quicker beat; the board narrows the swing
eve-bridge move --stop
```

Rhythms: `sway`, `dance`, `nod`, `shake`, `bounce`, `wiggle`, `fig8`.
Gestures: `doubletake`, `shrug`, `tilt`, `perk`, `droop`, `lean_peek`, `home`.
The host-side table is `bridge/src/eve_bridge/motion.py`. The plan that
carries the velocity-gate evidence is `docs/plans/eve-motion-control.md`, which
stays local: `docs/plans/` is not tracked.

**The board decides what is safe, not the host.** A request for 120 degrees at
10 Hz comes back as 15 degrees at 1.5 Hz rather than being obeyed or refused. So
the host table is a set of friendly starting points and nothing more — `dance`
asks for 25 degrees rather than 28 because 28 is what the board reduces it to.

**Elapsed time is computed with `ticks::elapsedMs`, never by subtraction.** The
oscillator's start is stamped by the consumer, which runs after `loop()` captured
`now`, so a plain `now - oscStart` underflows to about 4e9 ms on the first tick
and the motion ends before it starts.

## Taking notes

`eve-bridge take-notes start|stop|status|list`, or say "start taking notes".
See [voice.md](voice.md#taking-notes-on-the-room). The recorder subscribes to the
same microphone stream the wake word uses, so eve is deaf to its own name while
recording and learns to stop by reading its own transcript.

## Daemon setup (this machine)

```bash
cd bridge && .venv/bin/eve-bridge install                       # hooks
.venv/bin/eve-bridge install --service --serial-port '/dev/cu.usbmodem*'
.venv/bin/eve-bridge install --notes-widget                     # optional pyobjc panel at login
```

The board never gates a permission — the swipe card is gone, so every prompt
defers to Claude Code's own terminal prompt and taps only focus the terminal.
(`EVE_MONITOR_ONLY` used to force this and is no longer read.)
`~/.config/eve-bridge/matchers.toml` has `replace_defaults = true` and an
empty `always_ask`, so no Bash command is routed to the board either.

The launchd daemon does not read `.zshrc`: put `OPENROUTER_API_KEY=...` in
`~/.config/eve-bridge/env` (mode 600); a value already in the environment
wins. Eve calls every model with that key ([one key: OpenRouter](../openrouter.md));
the desk voice alone still needs an `OPENAI_API_KEY` there, until it moves too. The daemon's python needs **Input Monitoring** (listen key) and
**Accessibility** (the Enter/arrow taps a swipe sends); the startup log names
the binary.
Reload the plist with `launchctl unload` + `load -w`, not `kickstart`.

Knobs (all in `bridge/README.md`): `EVE_LISTEN_KEY` (`option`, `fn`,
`off`), `EVE_OWNER_THRESHOLD` (0.9, lower is stricter), `EVE_EXPLORE=0`,
`EVE_EXPLORE_AFTER_MIN` (10), `EVE_EXPLORE_CYCLE_MIN` (15),
`EVE_NOTES_PER_HOUR` (6), `EVE_NOTES_MODEL` (`gpt-6-luna`),
`EVE_NOTES_DIR`, `EVE_SAVE_FRAMES`, `EVE_PHOTOS_MAX` (300),
`EVE_PHOTOS_MAX_MB` (100); the voice and computer-control
knobs (`EVE_VOICE`, `EVE_WAKE_WORD`, `EVE_COMPUTER_CONTROL`,
`EVE_AGENT_MODEL`, …) are in [voice.md](voice.md). The daemon's python also
needs **Microphone** (wake word) and **Screen Recording** (computer use).

## Owner identity and explorer

Hold Option while facing the robot for a few seconds: the daemon takes one
Vision feature print (`VNGenerateImageFeaturePrintRequest` revision 2) every
2 s of the single face in frame, keeps up to 24 in
`~/.config/eve-bridge/owner_faceprints.json` (mode 600), and tags every
later face `owner` or `unknown` (`eve-bridge identity`, `identity reset`).
After 10 idle minutes the daemon sends `mode explore true`, walks ten `look`
waypoints (yaw -45..45 at pitch 40, then 60), and when a view changed spends
one note (OpenAI Responses API, one low-detail image, low reasoning, 1,024 total output tokens max)
on `~/.config/eve-bridge/notes/YYYY-MM-DD.md`. Between the host's looks
and through the rest between cycles the robot stays in explore mode and looks
around the room on its own. Any sign of the human stops it. `eve-bridge notes --last 3` reads them; the WidgetKit widget in
`widget/` or `eve-bridge notes-widget` shows them on the desktop.

You can also send it off by hand: `eve-bridge explore` (`status`, `stop`)
over IPC (`{"evt":"explore","action":"start|stop|status"}`), or say
*"hey eve, go explore"* — the voice tool `go_explore` ends the conversation and the daemon
starts the explore once the board has left the conversation pose. A manual
explore ignores the idle clock; a touch, the listen key, the next wake
word, a disconnect or `explore stop` ends it. Exploring is explicit: the idle
start is **off** unless `EVE_EXPLORE=1` (owner decision, 2026-09-21).

When a view scores high on the cool factor the daemon sends `{"cmd":"snap"}` and
keeps the 320x240 frame the board answers with as a photo under
`photos/YYYY-MM-DD/` in the notes directory (`eve-bridge photos`). The score,
its research and the habituation rules are in [personality.md](personality.md) § 4.

## Bench-verified conventions

- BSP `+yaw` turns the head to the **robot's right** = the viewer's left. A
  toucher at the screen's right is reached with a negative yaw (-28).
- GC0308 RGB565 is **big-endian per pixel** (`kRgb565ByteSwap = true`); the
  JPEG encoder reads it the same way, so the subsample copies words untouched.
- Static init: sprites must take `&halDisplay()`, never `&M5.Lcd`. `M5.Lcd` is
  a reference member of `pet::M5`, constructed in another TU; reading it at
  static init handed the sprite a null parent and boot-looped the first flash.
- Two globals named `M5` coexist (M5Unified's and the pet's `M5Compat`). The
  TUs that include M5Unified never include `board_compat.h` (`src/hal_m5.h`).
- The HWCDC RX/TX rings are raised to 4096 before the first `Serial.begin()`.
