# Past experiments — design notes (Freenove FNK0104B touch pet, CrowPanel e-ink)

The robot itself (M5StackChan) is documented in [../DESIGN.md](../DESIGN.md) and
[../docs/stackchan/build.md](../docs/stackchan/build.md). Everything below is the
history: the boards eve grew out of. Paths are relative to this folder unless they
start with `../`.

A desk pet that reacts to Claude Code activity, built by forking open-source repos:

- **Firmware base:** [anthropics/claude-desktop-buddy](https://github.com/anthropics/claude-desktop-buddy) (MIT) — official 7-state pet
  (`sleep/idle/busy/attention/celebrate/dizzy/heart`), 18 ASCII species, GIF character packs,
  NDJSON wire protocol, and — key — **USB serial ingest already implemented** (`data.h` feeds `Serial`).
- **Host bridge base:** [SnowWarri0r/eve-bridge](https://github.com/SnowWarri0r/eve-bridge) (MIT) —
  Claude Code CLI hooks → Unix socket → daemon → device, with the documented state mapping
  (`total`←SessionStart/End, `running`←UserPromptSubmit/Stop, `waiting`+`prompt`←PreToolUse,
  tokens/messages←`~/.claude/projects/*.jsonl` tailer). We add a **serial transport** beside its BLE one.
- **Board pin data:** [Freenove/Freenove_ESP32_S3_Display](https://github.com/Freenove/Freenove_ESP32_S3_Display) (official vendor repo).

## Board facts (verified)

| Subsystem | Details |
|---|---|
| MCU | ESP32-S3 QFN56 rev0.2, 16MB QIO flash, 8MB **OPI** PSRAM, native USB-Serial/JTAG (`/dev/cu.usbmodem101`) |
| LCD | ILI9341(V) 240×320 SPI40MHz — MOSI=11 SCLK=12 MISO=13 CS=10 DC=46 RST=-1, BL=GPIO45 **active HIGH**; needs `ILI9341_2_DRIVER`, `TFT_INVERSION_ON`, `TFT_RGB_ORDER TFT_BGR` |
| Touch | FT6336G capacitive @I2C 0x38 — SDA=16 SCL=15 INT=17 RST=18 |
| RGB LED | 1× WS2812B GPIO42 (GRB) |
| Audio | ES8311 codec + FM8002E amp (I2S MCLK4 BCLK5 WS7 DOUT8 DIN6, amp-en GPIO1) + onboard mic — **deferred to later phase** |
| SD | 4-bit SD_MMC only (CLK38 CMD40 D0=39 D1=41 D2=48 D3=47) — unused |
| Battery | ADC GPIO9, ÷2 divider; TP4054 charger |
| Build | FQBN `esp32:esp32:esp32s3:CDCOnBoot=cdc,FlashMode=qio,FlashSize=16M,PSRAM=opi,PartitionScheme=huge_app` — 3MB app at 0x10000, LittleFS on the 896KB `spiffs` partition at 0x310000 |

USB-serial note: opening the S3 native USB CDC port does **not** reset the sketch (unlike UART-bridge boards), so a host daemon holding the port open is safe. It does hold it *exclusively*, though — unload the daemon before flashing or esptool cannot connect.

Partition note: `huge_app` is a 4MB layout on a 16MB part, so ~12MB of flash is unaddressed and LittleFS gets 896KB. That is enough for the GIF character packs today. Moving to `default_16MB` (6.25MB app ×2 OTA + 3.5MB LittleFS) means relocating `spiffs` from 0x310000 to 0xc90000, so it needs a full `esptool erase-flash` first — see the partition gotcha below before attempting it.

## Architecture

```
Claude Code CLI ─hooks(async)→ unix socket → eve-bridge daemon ─┬─ BLE NUS (kept, works on S3)
        └─ statusline/JSONL tailer (tokens, messages) ───────────────┴─ NEW: USB CDC serial NDJSON
                                                                            ↓ /dev/cu.usbmodem101
                                             firmware (fork of claude-desktop-buddy)
                                             data.h NDJSON parser → TamaState → derive() → pet states
                                             touch: swipe card right=approve · left=deny ·
                                             swipe card up=focus terminal · tap pet
                                             (attention)=focus terminal · hold pet=dictate ·
                                             swipe down=Enter · tap-right=scroll transcript
```

## Firmware port map (M5StickC Plus → FNK0104B)

| M5 dependency | Replacement |
|---|---|
| `M5.Lcd` (135×240 ST7789) | `TFT_eSPI` w/ custom setup header; W=240 H=320; sprite in PSRAM |
| `M5.BtnA/BtnB` | `TouchBtn` compat class over FT6336U: left/right tap zones, same `wasReleased/pressedFor` API |
| `M5.Imu` shake/face-down/orientation | dropped; dizzy = fast scrub gesture on pet, nap = tap-and-hold on sleeping pet; clock fixed portrait |
| `M5.Rtc` | ESP32 system clock (`settimeofday` from bridge `{"time":[...]}` sync) |
| `M5.Axp` brightness/power/battery | LEDC PWM on GPIO45; battery = `analogReadMilliVolts(9)*2`; power off → backlight off |
| `M5.Beep` | stub (later: ES8311 I2S chirps) |
| red LED GPIO10 | WS2812 GPIO42 via `neopixelWrite()` — attention=pulsing orange, heart=pink, celebrate=rainbow |
| BLE bridge | kept as-is (ESP32 BLE works on S3) |

Everything else (data.h, stats.h, xfer.h, character.cpp GIF renderer, 18 species) ports unchanged
apart from geometry constants (`EVE_X_CENTER` 67→120, canvas 135→240, layout scale).

## Swipe-card approvals

Permission prompts render as a draggable card (`main.cpp`, `CARD_*` constants + `drawApproval`)
instead of the upstream tap-left/tap-right panel:

- **Input** comes from the raw FT6336 state (`M5.touching()/touchX()/touchY()`), not the
  synthetic `BtnA/BtnB` zones. A drag can start anywhere in the card band (y ≥ 204). While a
  prompt is visible the button handlers and pet gestures are swallowed — the tail of a swipe
  crossing the strip must not cycle screens, and a drag through the pet zone must not scrub.
- **Tilt** is real rotation: the card face is drawn into a second 210×80 sprite and composited
  into the full-screen sprite with `pushRotated` (±9° clamp, `TFT_TRANSPARENT` corners). At ±9°
  the rotated half-height is ~56px, so a center at y=262 keeps every pixel inside the 204..320
  band, which is cleared each frame — same self-clearing model as the old panel, no trails over
  the dirty-region pet renderer.
- **State machine** `REST → DRAG → FLY | SNAP`: release past 60px (or a >10px/frame flick)
  sends the decision (`once`/`deny`) immediately and accelerates the card off-screen (×1.12/frame);
  otherwise it springs back (×0.65/frame). The stamp + border color flip at 30% of the commit
  distance, with a beep latch at 100%.
- **Memory:** the card sprite (33KB) allocates on prompt arrival and frees on resolve; if
  allocation fails the card draws directly on the main sprite, untilted, and swiping still works.

## Terminal focus ("take me there")

Two gestures send `{"cmd":"focus"}` board→host; both are look-only and decide nothing:

- **Swipe up on a permission card** carries the prompt id — the daemon resolves the asking
  session's cwd from `_pending_cwds` and the card stays pending.
- **Tap the pet in attention state** carries no id — the daemon resolves the waiting session
  itself (`State.attention_cwd()`): the oldest pending permission's cwd, else the most recently
  flagged needs-input session's. The tap is gated on `baseState == P_ATTENTION` (not
  `activeState`, so a one-shot celebrate overlay can't eat it) and stays wake-only in every
  other state — touching the pet remains functional-only.

`focus_terminal.py` then raises the terminal, best match first:

1. **iTerm2 / Terminal.app** — searched tab-by-tab for the cwd basename in titles (AppleScript).
2. **AXRaise pass** — every *running* app from the activate list (Ghostty, Warp — whose
   executable is literally named `stable` — cmux `com.cmuxterm.app`, Composer, Cursor, VS Code)
   gets its window titles read through System Events/Accessibility; a title containing the cwd
   basename is AXRaise'd and the app fronted. This is how window-level focus works on apps with
   no AppleScript model.
3. **Already-frontmost check** — if a candidate is frontmost and nothing matched by title, the
   daemon logs "assuming it's on screen" and stops. Claude Code retitles windows to the
   conversation summary (never the cwd), so a session you're already looking at can't title-match;
   activating would be an invisible no-op — the 2026-08-07 "tap didn't work" confusion.
4. **App-level activate** — first running candidate is raised whole.

`EVE_FOCUS_APPS` (comma-separated names, priority order) replaces the activate list; names
matching a default keep their bundle id, unknown names activate by name. Needs macOS Automation
permission for the scripted apps + System Events (prompted once); every failure is non-fatal —
worst case nothing raises.

## Diagnostics

A frozen board used to tell you nothing: the screen is stale and the single USB CDC pipe (which
the daemon owns exclusively) just goes quiet. UART0 (43/44) is free but needs a USB-TTL adapter
wired on, so `src/diag.h` takes the no-extra-hardware route instead — an event ring in
`RTC_NOINIT` memory (survives panics, watchdog reboots and software resets, not power loss), the
decoded `esp_reset_reason()`, and a task watchdog on `loop()` so a true hang reboots and reports
rather than sitting there silently. A one-byte phase marker per loop stage names the call that
never returned, and any iteration ≥250ms is logged with the stage that ate the time.
`eve-bridge diag` decodes all of it on the next boot.

Two hangs are still open, distinguished by the phase marker: `DIED IN: loop end` (loop task
stopped being scheduled inside `delay()`, no slow iteration beforehand, cause unknown) and
`DIED IN: render` (a 240×320 16-bit `pushSprite` is ~153KB over SPI at 40MHz, ~31ms). Both
self-heal — the watchdog reboots within 15s.

Disconnect runbook (from the 2026-08-05 crash-loop investigation):

- `[alive]` every 5s is unconditional only **while `loop()` runs**. A silent board on a port
  that still enumerates and opens means the CPU is hung or rebooting — the USB-Serial/JTAG
  peripheral is autonomous silicon and keeps enumerating regardless. `diedIn` in the next
  boot's diag names the stuck phase; the daemon now logs it.
- The daemon escalates on its own: an ack-less link forces a reconnect with a 15s closed-port
  hold; repeated silent reopens or a second ack-less cycle RTS-reset the board. **Physical
  replug is never the runbook** — and it wipes the `RTC_NOINIT` diag ring, so if you must
  replug, run `eve-bridge diag` first and save the output.
- Every `tools/flash.sh` run archives the exact ELF under `firmware/build-archive/` (both under `past-experiments/`) so a
  panic backtrace stays symbolizable (`xtensa-esp32s3-elf-addr2line -e <elf> <addrs>`). The
  2026-08-05 05:35 backtrace was lost because the only ELF postdated the crash.
- The suspected hang site was the WS2812 write: the core's `rgbLedWrite()` ends in
  `rmtWrite(..., RMT_WAIT_FOR_EVER)`. `ledSet()` now encodes the frame itself with a 100ms
  deadline and skips the frame on timeout.

## Shell (case/)

`case/shell_v2.py` is the current parametric FreeCAD script (run via `freecadcmd`, exports STLs +
FCStd to `case/export/`; `case/shell.py` is the superseded v1 record). Parts, all support-free in
their print orientations: **frame** (face down — bezel + walls; 4× M3×14/16 flat-head machine
screws enter countersinks from the front, ride D-trimmed guide standoffs through the PCB holes),
**back** (outer face down — bosses seat the PCB, biased 0.3mm short of the derived PCB plane so
stack-estimate error can pull the glass back but never crush it into the lip; each boss carries a
Ø4.0 × 6.8mm blind bore for an **M3 heat-set insert**, pressed flush from the boss top, up to
M3×5.7 with 0.4mm outer skin left; corner lips register the plate; WS2812 glow window +
BOOT/RESET pokeholes), **gauge** (a 1.2mm board-footprint plate with the hole grid — print it
first and verify the bare PCB's holes show daylight before printing the shell), **stand** (v1,
unchanged and still fits — base down, 65° wedge dock, open cable mouth; shell_v2 asserts the
outer envelope is unchanged so a future edit that breaks stand fit fails the build). Portrait,
USB-C edge down; long walls carry relief pockets for the edge-mounted JST sockets, and the USB
notch continues through the back-cover edge so chunky cable overmolds (~8.5mm) pass.

**The script proves fit on every build**: a mock board (PCB + display module + USB body + JST
overhangs + measured component envelope with hole keep-outs) must intersect neither shell part or
the build asserts. Measured 2026-08-07: board 85.95×50.99, glass 69.69×50.11, back envelope 10.66
(glass→tallest component; the earlier 9.50 was discarded), USB-C 9.10 wide +1.45 toward BOOT.
Re-measured after the first print (whose holes were off): hole grid **77.18×41.50**
center-to-center (v1's 74.95×39.59 was >2mm wrong on both axes), edge→hole-edge 2.43 (cross-check;
disagrees with the direct pitch reading by ~0.7mm, which is why the gauge part and the +0.2 slop
in the frame's screw bores exist), PCB seat 1.69 thick. Derived: PCB plane at 7.46 (envelope −
3.2 USB body). Eyeballed, oversized to absorb error: button/mic/LED positions. The `freecad` MCP
server is registered in `.mcp.json` for live-in-GUI iteration.

## Gotchas

- **Touch releases need debouncing.** `readTouch()` returns false on a momentary `TD_STATUS==0`
  or any I2C hiccup mid-press. Treating that as a release fired a tap, then the next poll saw the
  finger again and began a fresh press — one finger produced ~6 taps/second, and the storm wedged
  the loop task into a watchdog reset. `TOUCH_UP_POLLS` consecutive empty reads are now required
  before believing a release; any new gesture must debounce the same way.
- **LittleFS formats itself on first boot.** A freshly flashed board's `spiffs` partition has
  never been formatted and `LittleFS.begin(false)` will not format it — it stays at `fsTotal=0`
  forever and the daemon rejects every character push. `characterInit()` formats once when the
  mount genuinely fails; safe, since the partition only holds re-pushable GIF packs.
- **Partition gotcha.** A sketch-local `partitions.csv` is silently ignored by arduino-cli +
  esp32 3.3.10 — the flash ends up with a stale default table and an unbootable app. Use an
  explicit `PartitionScheme`, and `esptool erase-flash` when in doubt.
- **HWCDC needs DTR.** The S3 drops all `Serial` TX unless the host asserts DTR — `cat` sees
  nothing, pyserial with `dtr=True` does.
- **A one-way serial link looks healthy.** USB re-enumeration (every board reset, including
  esptool's) leaves the host holding a file descriptor that no longer reaches the device: writes
  succeed into the void, reads return empty, nothing raises. The firmware prints `[alive]` every
  5s and the transport forces a reconnect after 20s of silence.
- **Clock glyph padding is load-bearing.** Both faces centre the time and repaint only their own
  glyph cells, so the 5→4 character shrink at 12:59 → 1:00 would strand pixels. The hour is
  space-padded, and the leading space draws as a background-filled cell that clears them.
- **The host allowlists exactly one key.** `{"cmd":"key","name":...}` accepts only `enter` — a
  peripheral on a serial line asking for arbitrary keystrokes is a much larger surface than this
  needs. It is also refused during a push-to-talk hold, where Option is still down and a bare
  Return would arrive as Opt+Return.
- **Held modifiers are released defensively.** A stuck modifier breaks typing system-wide, so the
  daemon force-releases after 60s if the release event is lost, and always on shutdown. Releases
  carry `flags=0` — asserting the modifier flag on key-up tells macOS the key is still down.
- **Bash matchers are anchored at the start of the command** (`^git push( |$)`, …). A leading
  variable assignment or `cd` defeats them, and the command quietly takes the default path.

## E-ink port (CrowPanel 4.2")

`firmware/claude_pet_eink` — same NDJSON protocol, second board. Not a
board_compat shim: e-paper invalidates the whole render model (a 240×320
sprite pushed 30×/s), so the sketch is a small event-driven remake (~500
lines + the vendored Elecrow SSD1683 driver). **There is no pet on this
build** — the first revision drew the ASCII cat, but on a panel that redraws
once a minute a mascot is mostly a ghosting liability, so it's a purely
functional portrait status display (`UI_ROTATE 90`, flip to 270 if a future
dock stands it the other way): big clock + date, state banner
(IDLE/WORKING/NEEDS YOU/DONE!), session/token counters, transcript tail,
full-screen permission cards.

**Board facts (verified 2026-08-11):** Elecrow CrowPanel ESP32 4.2" E-Paper
HMI. ESP32-S3-WROOM-1-N8R8 (QFN56 rev0.2, 8MB QIO flash, 8MB PSRAM — unused),
SSD1683 panel 400×300 1bpp. USB-C goes through a **CH340** to UART0, so the
port is `/dev/cu.usbserial-*` and opening it can auto-reset the board (unlike
the FNK's native CDC). Flash at **460800** — 921600 dies mid-write on the
CH340. Pins: EPD SCK=12 MOSI=11 RES=47 DC=46 CS=45 BUSY=48 (bit-banged),
panel power GPIO7 HIGH. Buttons active-low, external pull-ups: MENU=2 EXIT=1
rotary up=6 down=4 press=5. FQBN
`esp32:esp32:esp32s3:FlashSize=8M,PartitionScheme=default_8MB` — USB CDC off,
`Serial` is UART0.

**Refresh policy.** Every render clears the full 15KB framebuffer and redraws
from scratch (no incremental dirty regions — so text overlap is impossible at
the buffer level), then pushes the whole screen: partial refresh normally,
fast-full (`EPD_Init_Fast` 1.5s waveform) on boot, on card in/out (big
inversions ghost worst) and every 24 partials to deghost. Panel deep-sleeps
after every push; every render path re-inits, which wakes it. Redraws are
triggered by a change signature — pet state, session counts, prompt id/queue/
ttl-bucket (5 buckets, so a card redraws ~5× over its 300s TTL), connection,
and the minute tick (which also alternates the two art frames). Token counts
ride along on the next tick rather than triggering one. Steady state is one
partial per minute ≈ 1.4k refreshes/day against a ~1M-cycle panel.

**Contract kept with the daemon:** `[alive]` every 5s (RX_SILENCE watchdog),
`{"ack":"status",...}` in the touch build's exact shape (ack watchdog + CLI
`status`; battery is faked at 100%/USB — the BAT socket has no sense line),
time sync consumed, `permission`/`focus`/`key` verbs emitted. `char_begin` is
refused at the handshake (`ok:false`) so the host never streams a GIF pack at
a board with no filesystem — the daemon's "LittleFS unformatted" ERROR on
connect is this refusal being misread and is cosmetic. `Serial` RX buffer is
raised to 4096 *before* `begin()`: a render blocks `loop()` for ~1–2s and a
2KB heartbeat must survive it (UART default is 256).

**Buttons replace gestures.** With a card up the slider decides like the
touch build's swipe — up = approve once, down = deny (a flick can't "hold",
so destructive approvals route through the HOLD-OK gate instead). OK
short/hold = approve once/always (hot prompts require the hold; a short tap
draws a "HOLD OK" hint — the physical analogue of the stiffer swipe), EXIT =
deny with a card up and
**hold-for-push-to-talk without one** (`{"cmd":"voice"}` start on the down
edge, stop on release; the prompt-ness is latched at press time so a card
arriving mid-dictation can't turn the release into a deny), MENU = focus
(with prompt id when a card is up), rotary = `prev`/`next` keys, OK with no
card = `enter`. Presses land on release with a 30ms debounce; a tap fully
inside a refresh window can be missed — known v1 limit.

**Local changes to the vendored driver:** (1) `EPD_ReadBusy()` got an 8s
escape (stock code spins forever; a wedged panel would kill `[alive]` and
the daemon would RTS-reset us — which on the CH340 wiring is a real EN
reset, so it recovers, but the timeout makes it a non-event). (2) **The
partial-refresh overlap autopsy.** The SSD1683 computes a partial refresh as
the *diff* between its new-data RAM (0x24) and old-data RAM (0x26); pixels
where the planes agree are not driven, so a previously-black pixel the diff
misses stays black and consecutive screens' text visibly merges ("19:88"
clocks, interleaved words — first-print photos, 2026-08-11). The stock
Elecrow driver mishandles this, and the first fix attempt (write 0x26 after
each partial) made it differently wrong: per the SSD1683 datasheet (§0x37
"Ping-Pong for black/white mode", enabled via panel OTP) the controller
**switches its plane roles after every Mode-2 update**, so a post-update
write to 0x26 alone lands in the plane about to serve as *current* and the
true old plane goes stale — the merge persisted. (RAM itself provably
survives deep sleep mode 1 + HW reset + SWRESET — datasheet: "Retain RAM
data but cannot access the RAM"; resets clear register state, "RAM are
unaffected".) The reference drivers use exactly two safe patterns: never
touch 0x26 after the baseline full refresh and let ping-pong maintain it
(Good Display / Waveshare / the Elecrow factory firmware, whose partial wake
is a bare RES pulse), or rewrite BOTH planes after every refresh ("set
current and previous buffers equal" — GxEPD2). We adopted the GxEPD2 recipe
verbatim (2026-08-11, verified on hardware): **(a)** power the analog stage
down before deep sleep (`0x22=0x83` + `0x20` + busy, then `0x10=0x01`);
**(b)** rewrite both planes after every partial; **(c)** wake for a partial
with `EPD_Wake()` (RESET pulse + SWRESET + `0x18=0x80` internal temp
sensor), never the full `EPD_Init()`; **(d)** trigger partials with
`0x22=0xFC` forcing `0x21=0x00,0x00` immediately before, so the old plane is
never bypassed (`0x12` resets 0x21 even though it leaves RAM alone). Related
sharp edge from GxEPD2's own v1.5.6 fix on this exact panel: any `0x22`
value that loads the Mode-2 LUT outside a real display update (0xF8, 0x99,
0xB9) triggers the buffer switch on its own — don't. Ghosting proper (faint
residue the waveform can't fully erase) still exists and is what the
24-partial full-refresh cycle clears.

**Verified 2026-08-11 with hwlog** (`~/Documents/personal/hardware-logging`):
state machine transitions, status ack, prompt card, char refusal, button
verbs, plus a conditioning suite — 30-message state flap at 400ms (renders
coalesce via the signature check, no queue), 26 alternating partials across
the deghost boundary, 4× max-length hot cards (720-byte detail wrapped), the
23:59→0:00 clock shrink, and a >4KB heartbeat flood during a render. Zero
crashes, heap flat at 328480 through the whole run.

## Phases

- **A — display bring-up:** sketch compiles under arduino-cli, eve idle animation renders correctly
  (colors, inversion), demo mode cycles states. Flash + visual verify.
- **B — input port:** touch buttons, gestures, approval screen at 240×320.
- **C — host bridge:** vendored eve-bridge + `serial_transport.py` (pyserial), venv install,
  hook installation into `~/.claude/settings.json` (with user approval), end-to-end verify.

## Licenses

Firmware fork: MIT (Anthropic PBC) — preserved. Bridge fork: MIT (Snow) — preserved.
`characters/bufo` GIFs are third-party art — not redistributed here.
