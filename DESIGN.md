# eve design notes (M5StackChan K151)

The robot in the top-level README. The boards it grew out of — the Freenove
touch pet and the CrowPanel e-ink builds — keep their design notes in
[past-experiments/DESIGN.md](past-experiments/DESIGN.md).

## StackChan port (M5StackChan K151)

`firmware/claude_pet_stackchan` — third board, same NDJSON protocol, same
`data.h`. Unlike the e-ink remake this is the pet build with a new
`board_compat` layer: the sprite/HUD pipeline is unchanged, the ASCII
species and GIF renderer still compile but are not drawn, and the face is
FluxGarage RoboEyes. Full reference: `docs/stackchan/build.md`.

**Board facts (bench 2026-09-05):** CoreS3 core (ESP32-S3, 16 MB flash, QSPI
PSRAM), native USB-Serial/JTAG at `/dev/cu.usbmodem*` — a plain port open does
**not** reset the sketch. ILI9342C 320×240 landscape + FT6336U touch via
M5Unified. GC0308 camera. Two Feetech SCS0009 servos on UART1 (G6/G7,
1 Mbaud) through StackChan-BSP `Motion`. 12 WS2812C written as RGB565 through
the PY32L020 expander. Si12T three-zone top touch. FQBN
`esp32:esp32:m5stack_cores3:PartitionScheme=huge_app,PSRAM=enabled`; libraries
M5Unified 0.2.21, M5GFX 0.2.28, StackChan-BSP 1.1.0 and RoboEyes 1.1.2
(symlinked from `vendor/stackchan/`), IRremoteESP8266, M5Unit-NFC. Factory
image backed up to `firmware/build-archive/stackchan-factory-20260905.bin`;
restore with `esptool write-flash 0x0`, never with `-b` (a baud switch stalls
this link).

### Port map (pet build → K151)

| Pet dependency | Replacement |
|---|---|
| `TFT_eSPI` + FT6336 compat | `M5.Display` (M5GFX) and `M5.Touch` behind `hal_m5.h` — the pet never sees M5Unified's `M5` |
| ASCII species / GIF face | `eyes.cpp`: RoboEyes on a 320×204 1-bit `LGFX_Sprite`, pushed into `spr` each tick; random hue per boot; status word; no clock |
| Single WS2812 | `body.cpp` LED table through `ledSet()` (unchanged colours skip the I2C write) |
| `M5.Beep` stub | `chirp.cpp`: R2D2 phrases synthesized to 8-bit PCM at 16 kHz in PSRAM, `M5.Speaker.playRaw()` |
| No body | `body.cpp`: keyframe sequences per state, `easeInOutCubic` tween streamed at 25 Hz over the BSP spring (auto-angle-sync off), micro-drift while awake, torque release at rest |
| Touch strip BtnB | also the back top zone; front zone tap = pet tap |
| `M5.Rtc` | BM8563 through `halRtcGet/Set`, system clock fallback |
| `derive()` | Claude idle → SLEEP, any session running → BUSY, waiting → ATTENTION; IDLE means "no daemon" |

Two globals named `M5` coexist (M5Unified's and the pet's `M5Compat`). Only
`hal_m5.cpp`, `body.cpp`, `gaze.cpp`, `chirp.cpp` include M5Unified, and none of
them include `board_compat.h`. Sprites take `&halDisplay()`, never `&M5.Lcd`:
`M5.Lcd` is a reference member constructed in another TU, so reading it at
static init handed the sprite a null parent and boot-looped the first flash.

### Bench-verified conventions

- BSP `+yaw` = the robot's RIGHT = the viewer's left. A toucher at the
  screen's right is reached with yaw -28 (`YAW_FOUND_YOU`).
- GC0308 RGB565 is big-endian per pixel (`kRgb565ByteSwap = true`); the JPEG
  encoder reads the same order, so the stream subsample copies words as-is.
- Camera HFOV 66° is an assumption. `kYawSign` +1 verified (head follows the
  hand); `kElevSign` is unverified.
- Top touch is armed 3 s after boot: the Si12T baseline is stale while the
  servo rail rises (the middle zone read pressed at boot).
- Pitch is clamped to 5..85 (M5Stack's advice), yaw to ±60 for the cable loom.

### Gaze policy (`gaze.cpp`)

Priority: host `face` cmd (conf ≥ 40, absolute angles from the pose echoed in
the cmd; host silent 3 s → fallback) → on-board motion/skin blob (`look.cpp`,
core 0, frames near a move quarantined) → owner memory (`owner_model.h`, a
habit histogram in NVS `owner`, saved every 5 min or on confidence ≥ 60).
Owner wanted (ATTENTION or listening): live → remembered spot → two-row
search sweep (pitch 45 then 65, yaw -40..40) every 5 s until a live target is
younger than 5 s. BUSY/IDLE glance only (8° deadband, every 3 s). An
`unknown` face gets a glance and never trains memory. SLEEP and one-shots
never move for the camera. `look` from the host is honoured in
SLEEP/IDLE/BUSY only, never while the owner is wanted.

### Wire additions

host→board `{"cmd":"listen","on":bool}` (Option key), `{"cmd":"cam","on":bool,
"fps":5,"w":160,"h":120}`, `{"cmd":"face","seq":n,"bx":±100,"by":±100,"size":
0..100,"conf":0..100,"yaw":Y,"pitch":P,"who":"owner"|"unknown"}`,
`{"cmd":"look","yaw":±60,"pitch":5..85,"hold":ms}`, `{"cmd":"mode","explore":
bool}`, `{"cmd":"owner","op":"reset"}`, `{"cmd":"agent","state":"wake"|"listening"|
"thinking"|"speaking"|"working"|"asking"|"done"|"error"|"idle"}` (the host
voice / computer-control conversation; `body.cpp` / `eyes.cpp` act each phase
out), `{"cmd":"emote","dv":±100,"da":±100,"label":".."}` (the diary's
appraisal → `mood.cpp`, clamped to ±0.3), `{"cmd":"caption","page":i,"of":n|0,
"lines":[".."],"hold_ms":ms,"chirp":bool,"final":bool}` / `{"cmd":"caption","clear":true}`
(one pre-wrapped page of eve's reply; the host's `caption_pager.py` owns the
pacing, the board draws, chirps once per page and parks the eyes on the N row;
the Mac stays silent unless `EVE_VOICE_OUTPUT=audio`). board→host `{"frame":{"seq":n,"w":160,
"h":120,"fmt":"jpeg","b64":"...","yaw":Y,"pitch":P}}`, one line per frame from
the camera task, paused while a character transfer owns the wire. The daemon
sends `cam on` once per connect after the time sync and `listen off` on every
connect so a reboot mid-hold never sticks the pose. `mode explore true` stays
on through the explorer's rest between pan cycles; with no `look` held the
body plays its own random look-around (`body.cpp`, `nextExploreAt`).

## Licenses

Firmware fork: MIT (Anthropic PBC) — preserved. Bridge fork: MIT (Snow) — preserved.
`characters/bufo` GIFs are third-party art — not redistributed here.
