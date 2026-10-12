---
title: Voice PE build
icon: 💡
order: 2
---

# eve on a Home Assistant Voice PE

`firmware/eve_voice_pe` runs eve on a stock **Home Assistant Voice Preview
Edition** (Nabu Casa NC-VK-9727). The Voice PE has no screen, camera or
motors. It acts as eve's lights, button, dial and chirps. It uses the same
newline-delimited JSON link and the same `eve-bridge` daemon as the
[StackChan](stackchan/build.md).

The **microphone stays on the Mac**. You talk by holding the button
([hold to talk](#hold-to-talk)). eve answers on the **Voice PE's speaker**
while it is the connected controller, and on the Mac speaker otherwise.

The Voice PE runs in one of two setups:

- **Next to the StackChan, as the controller** (the usual setup). The
  StackChan stays the robot: face, head, camera. The Voice PE is the control.
  Set its USB serial in `~/.config/eve-bridge/env` (see
  [Two boards](#two-boards-the-stackchan-and-the-controller)).
- **Alone, as the robot.** Leave `EVE_CONTROLLER_SERIAL` unset, and the
  daemon's single serial link picks the Voice PE. Set
  `EVE_VOICE_OUTPUT=audio` so that wake-word replies play on the Mac,
  because captions need a screen.

## What it does

| Input or state | Voice PE |
|---|---|
| Not connected (no message from the daemon for 30 s) | One dim dot circles the ring |
| Connected, nothing running | Asleep: three soft blue blips rise one after another (z z Z), then a dark rest |
| Claude sessions running, or eve working on a task | Teal comet |
| eve thinking | Violet double comet |
| eve listening (wake word, or the Listen key held) | Soft blue breathe, "hm?" chirp |
| eve speaking | Quick blue pulse |
| A session waits on you | Amber pulse, attention chirp when the count rises |
| Task finished | Green sweep, happy trill |
| Error | Red blink, falling boop |
| Hardware mute switch on | Two red LEDs at the top. Chirps are silenced. The Mac microphone is not affected |
| [Spotify mode](#spotify-mode) on | Slow green breathe. The button and the dial control Spotify |

| Control | While a session waits on you | Otherwise |
|---|---|---|
| Center button, hold | Talk to eve after 0.4 s. Let go to send | Talk to eve at once. Let go to send |
| Center button, quick tap | Presses Enter on the Mac (approve, pick the selected choice) | Too short to be words, so nothing is sent |
| Dial | Down / up arrow through the choices (clockwise = down) | Volume of eve's voice, 0–10, shown on the ring |

## Hold to talk

Hold to talk works like **Call eve** in the Telegram Mini App
([calling eve](stackchan/miniapp.md#calling-eve)), but uses the board's button:

1. Hold the button. The ring turns blue, and the daemon records the **Mac
   microphone** (`EVE_MIC`, the wake word's device).
2. Let go. The daemon transcribes the press and sends it to **the Telegram
   chat's brain**, as if you had typed it. The chat's tools all work, and the
   Telegram chat keeps a written copy. The ring spins violet while eve thinks.
3. eve reads the reply aloud on the **Voice PE's speaker**, or on the Mac
   speaker if no controller is connected when the call starts. The ring pulses
   blue. Press again while it speaks to stop it and talk. The dial sets the
   voice's volume.

The daemon side is `bridge/src/eve_bridge/desk_call.py`. It runs
`phone_call.Call` unchanged, with a stand-in for the phone's WebSocket that
routes the button, the Mac microphone and the Mac speaker. A call starts
on the first press. It ends 30 s after eve finishes, and the next press
starts a new call. While a phone call is in use (the Mini App's or
Eve Link's: a press, its answer or eve speaking, and 8 s after), the button
waits. A phone call that is merely open, or idle for 8 s, does not hold it:
a call takes eve's chat only from its first press, and an idle one gives
it back to the button (Eve Link keeps a call open for 45 s after a turn). A
wake-word conversation already has the microphone, so a press during one is
ignored, and the wake word is off while the button is held. The Telegram
chat and `OPENROUTER_API_KEY` must be set up, as they must for Mini App calls.

For the whole call the StackChan's head **follows whoever is talking**
([following whoever is talking](stackchan/vision.md#following-whoever-is-talking)).
The call tells the follower it is a conversation: `listening` from the first
press to the call's end (between presses too), `thinking` and `speaking` as
the ring shows them, and `idle` when the call ends. Before 2026-09-27 the
call's states went only to the ring, so the head never followed on a desk call.

On the wire, the board sends `{"cmd":"ptt","on":true}` on press and
`{"cmd":"ptt","on":false}` on release.

### eve's voice on the Voice PE

The speech is 24 kHz mono 16-bit PCM, the same audio the Mini App call
plays. `desk_call.BoardSpeaker` sends it to the controller only, in lines of
at most 100 ms:

- `{"cmd":"pcm","d":"<base64>"}`: about 6.4 KB each, about 64 KB/s in total.
- `{"cmd":"pcm_flush"}` drops what the board still holds when you interrupt.

The board decodes each line straight into a 12 s stream buffer in PSRAM,
skipping the JSON parser. The audio task waits for 150 ms of audio, then
plays it upsampled to 48 kHz at the dial's volume. The daemon paces itself
so it is never more than 3 s ahead of playback (`LEAD_SECS`), because
speech is made faster than it plays and the buffer is finite. The sound
setting (beeps off) does not mute the voice.

The `[alive]` line reports `voice_dropped`: bytes that arrived with the
buffer full. It stays 0 while the pacing holds.

## Spotify mode

In Spotify mode the Voice PE works like
[spotKnob](https://github.com/gurul/spotify-knob): the dial is the Spotify
volume and the button is the transport. It needs [Spotify](spotify.md) logged
in and the Voice PE connected as eve's controller.

**Turning it on.** Say "spotify mode" to eve, send it in Telegram, or tap
**Voice PE Spotify mode** on the Mini App's Music card. The ring breathes
green. "Spotify mode off", "exit spotify mode", or the Mini App button turns
it off, and so does holding the button for 3 s.

| On the Voice PE | In Spotify mode |
|---|---|
| 1 click | Play or pause |
| 2 clicks | Next track |
| 3 clicks | Previous track |
| Dial | Spotify volume, 4% per detent. The ring shows the level in green at once. eve sends the level to Spotify 0.4 s after the dial stops |
| Hold, then let go (0.6 s to 3 s) | Opens the device picker. A tick at 0.6 s tells you that you can let go |
| Hold 3 s | Leaves Spotify mode |

A click waits 0.35 s for the next one before it counts, so 1, 2 and 3 clicks
are told apart.

**The device picker** is spotKnob's picker. **Each device has its own
colour**: blue, orange, purple, cyan, yellow, pink or white. Green and red are
never used, because they mean Spotify mode and a refusal. While a device is
selected, the ring glows faintly in its colour, with a bright dot at its place
in the list. A device gets its colour the first time eve sees it and keeps
it while it keeps showing up (`~/.config/eve-bridge/spotify-colors.json`).
A device eve hasn't seen for 30 days, such as a guest's phone or a hotel TV,
is forgotten, and its colour goes back to the free ones. With more than seven
devices, colours repeat. When the music moves, the ring sweeps in the new device's
colour. The Mini App's Music card shows the key. The picker opens on the
device that is playing now. Turning the dial
moves through your Spotify Connect devices, and eve says each device's name
on the Voice PE's speaker. A click plays on the chosen device. A hold, or 10 s
with no input, backs out with nothing changed. Devices that Spotify marks as
restricted are left out. Without `OPENROUTER_API_KEY`, eve doesn't say the
names, and the ring is the only guide.

A green sweep with the "ok" chirp means Spotify took the command. A red blink
with the falling boop means it refused, for example when nothing is playing
on any device or the account isn't Premium.

While Spotify mode is on, the button doesn't do hold to talk, and a tap
doesn't press Enter on a waiting prompt. Leave the mode to get them back. The
mode is kept when the Voice PE reconnects, but turns off when the daemon
restarts. On the wire, the board sends `{"cmd":"music","clicks":n}`,
`{"cmd":"music","hold":"short"|"long"}` and `{"cmd":"music","dial":d}`. The
daemon sends `{"cmd":"music_mode","on":…}`, `{"cmd":"ring_level",…}` and
`{"cmd":"music_flash","ok":…}` to the Voice PE only. The code is
`bridge/src/eve_bridge/music_mode.py`.

### Language

eve transcribes Voice PE presses as English (`EVE_CALL_LANGUAGE=en`, the default). Before this was set,
the transcription model guessed the language of each press, and a short press was sometimes heard as another
language, so eve answered in that language. To use another language, set its ISO-639-1 code in
`~/.config/eve-bridge/env`. Set it empty to let the model guess again.

Even told "en", the model sometimes answers near-silence with a syllable in another script ("あ。" on a 7 s
phone call, 2026-10-08). With English set, a call drops words whose letters are mostly not Latin
(`phone_call.foreign`) and says "I didn't catch anything. Try again?", so eve is never asked in that language.

## Standard questions, answered by code

"What time is it", "what's the weather", "when is sunset", "what's 17
times 23", "convert 5 km to miles", "how many days until Christmas" and
"what's my battery" get no model turn and no web search. Code answers them
at once, with the clock, Open-Meteo (free, no key), a unit table or the
Mac's `pmset` (`bridge/src/eve_bridge/quick_answers.py`). The check
applies to everything the Telegram brain hears: texts, the chat window, Mini
App calls and hold to talk. Only a message that is **only** that question
counts. "What time is my meeting" or "weather in my photos" still goes to the
model. If a source fails, the model answers as before.

Light commands are handled the same way. "Lights off", "lights blue" or "dim
the lamp to 30%" is done by code, and anything more ("make it cozy") goes to
the model with the light tools. See [lights](lights.md).

Music commands work the same way. "Pause the music", "next song" or "music
volume 40" is handled by code, and anything more ("put on something chill")
goes to the model with the Spotify tools. See [Spotify](spotify.md).

"Here" is the phone's location while Eve Link sent it in the last hour
([phone location](phone-location.md)), else `EVE_WEATHER_PLACE` in
`~/.config/eve-bridge/env` (a place name), or `EVE_WEATHER_LAT` and
`EVE_WEATHER_LON`. A named
place ("weather in Tokyo", "time in New York") is looked up with Open-Meteo's
geocoder.

The button and dial send keys only while the daemon reports a waiting
session. The daemon also ignores them during a voice conversation.

Chirps follow eve's sound setting (`{"cmd":"sound"}`). When sound is off,
the board is silent.

## Two boards: the StackChan and the controller

Both boards are ESP32-S3s on the same `/dev/cu.usbmodem*` glob, so the
controller is named by its USB serial number. An ESP32-S3's serial is its
MAC. List the serials with:

```bash
bridge/.venv/bin/python -m serial.tools.list_ports -v    # "USB JTAG/serial debug unit", SER=...
```

Add the settings to `~/.config/eve-bridge/env`, then restart the daemon:

```
EVE_CONTROLLER_SERIAL=<the Voice PE's serial>
EVE_VOICE=0            # no "hey eve": you talk by holding the Voice PE's button
```

The daemon keeps its one robot link (`EVE_SERIAL_PORT`) for the
StackChan, and that glob skips the controller's serial. A second link
(`bridge/src/eve_bridge/controller.py`) opens the controller:

- **To the controller**, it copies the heartbeat, the time and the
  conversation state. So the ring lights up in sync with the StackChan, and
  it catches up after a reconnect or reboot. The beeps are the StackChan's
  alone. The owner's sound setting is not copied, and the controller is told
  `{"cmd":"sound","on":false}` each time it connects.
- **From the controller**, only `ptt`, `key` and `focus` reach the daemon.
  Its acks never reach the robot's status watchdog.

The ring and the StackChan's face both follow the call: listening while you
hold the button, then thinking, then speaking.

The flash scripts use the same setting. `tools/flash_voice_pe.sh` flashes only
that serial. `tools/flash_stackchan.sh` skips it. If two Espressif boards are
present and the setting is missing, both scripts refuse to guess.

## Hardware

| Part | Detail |
|---|---|
| Core | ESP32-S3, 16 MB flash, 8 MB octal PSRAM, native USB-Serial/JTAG (`/dev/cu.usbmodem*`, VID 0x303a PID 0x1001) |
| Audio | XMOS XU316 is the I2S clock master. The TI AIC3204 DAC is on I2C 0x18, and GPIO 47 enables the amplifier. The amplifier stays on, as in ESPHome. Switching it for each chirp made an audible click |
| LEDs | 12 WS2812 on GPIO 21, power enable on GPIO 45 |
| Button | GPIO 0, active low. This is also the boot strap pin |
| Dial | Quadrature on GPIO 16 / 18 |
| Mute switch | GPIO 3, HIGH = microphones cut |

`sb_i2s.*` and `sb_codec.*` come from `~/Documents/personal/homeboxV0`
(commit `9c8a4f8`). In that repository, `scripts/check-pins.mjs` and
`scripts/check-aic3204.mjs` compare them against the upstream ESPHome
firmware. This copy has two changes, both from the first run on a real
board. `sb_codec.cpp` waits 3 s after the XMOS reset, reads the DAC back,
and redoes the setup if the DAC lost it. `sb_i2s.cpp` counts failed
speaker writes for the diagnostics below. `sb_config.h` carries the same pin values.
The chirp recipes are the StackChan ones. Each phrase is rendered at 16 kHz
and written x3 to the 48 kHz bus.

## Flash

```bash
./tools/flash_voice_pe.sh                        # finds the Espressif port by USB VID
./tools/flash_voice_pe.sh /dev/cu.usbmodem101    # explicit port
```

The script compiles with
`esp32:esp32:esp32s3:FlashSize=16M,PSRAM=opi,PartitionScheme=app3M_fat9M_16MB,CDCOnBoot=cdc`
and archives the ELF under `firmware/build-archive/`. It stops the launchd
daemon, which holds the port, uploads, and restarts the daemon if it was
running. Libraries: ArduinoJson 7.4.3 and Adafruit NeoPixel 1.15.5 on ESP32
core 3.3.10.

If the upload cannot connect, hold the center button while you plug in USB.
This starts the ROM bootloader.

Then set the voice output in `~/.config/eve-bridge/env` and restart the
daemon:

```
EVE_VOICE_OUTPUT=audio
```

## Backup and restore of the stock firmware

The first flash replaces ESPHome. The full 16 MB flash was dumped on
2026-09-26 to `firmware/build-archive/voice-pe-factory-20260926.bin` (gitignored,
sha256 `d12eb03b0b142b5a3bf2538608147badc91cdf616d50f6eee35631602461e045`).
`esptool verify-flash` matched it against the chip before the first flash.

```bash
esptool -p /dev/cu.usbmodem101 write-flash 0x0 firmware/build-archive/voice-pe-factory-20260926.bin
```

The official web installer also restores ESPHome:
https://esphome.github.io/home-assistant-voice-pe/

**Dump quirk.** On this board, two 4 KB sectors (0x1cd000 and 0x321000)
stalled every stub-flasher read ("Serial data stream stopped"). They read
cleanly with `esptool --no-stub`. Read in 1 MB pieces, retry a failing
piece with `--no-stub`, then check the joined image with `verify-flash`.

## Audio diagnostics

Stop the daemon, which holds the port, then send these lines at 115200 baud:

| Line | Effect |
|---|---|
| `{"cmd":"tone","hz":660,"ms":2000,"level":0.6}` | Plays a sine even while chirps are off. Prints `[chirp] played in … ms: N bytes to i2s, E write errors` |
| `{"cmd":"dac","page":0,"reg":11}` | Reads one AIC3204 register. Add `"val":N` to write it first |
| `{"cmd":"amp","on":true}` | Sets the amplifier enable pin |

For a working setup, page 0 register 0x0B (NDAC) reads `0x82`, 0x1B (the
audio interface) reads `0x30`, and page 1 register 0x09 (output power)
reads `0x3C`.

**First bench run, 2026-09-26.** Every tone reached the I2S bus with zero
write errors, but nothing played. The DAC had NDAC and MDAC off, 16-bit I2S,
and page 1 at its reset defaults. The writes made straight after the XMOS
reset were lost while the XMOS booted. The speaker played after those
registers were rewritten. The fix is the 3 s wait and the readback check.

## The link

The daemon does not check which board is attached (`serial_transport.py`,
`daemon.py`). This board meets the two requirements it has:

- It prints an `[alive] … live= total= running= waiting= agent=` line every
  5 s. The daemon reopens the port after 20 s without any bytes.
- If the board stops taking writes (each write times out after 2 s), the
  daemon counts that reopen like a silent one. Three in 10 minutes reset the
  board through RTS on the next open. Before 2026-10-03 only silence counted,
  and a board that did not read was reopened every 5 s for two days.
- It answers `{"cmd":"status"}` with
  `{"ack":"status","ok":true,"data":{"name","owner","board":"voice-pe","snd","sys":{"up","heap"}}}`.
  It sends no `sec`, `bat` or `sys.fsTotal`: this board has no BLE, battery
  or LittleFS, and the daemon reads those keys only when they are present.

The board acks `sound`, `led`, `name`, `owner`, `species` and `unpair`. It
ignores the camera, head, face, caption and character-transfer commands.

## Limits

- The Voice PE microphones are not used. Streaming them to the daemon would
  need a new audio path over serial.
- eve's voice reaches the Voice PE, not the StackChan. The daemon picks
  the speaker as each call starts, so a controller plugged in mid-call
  takes over at the next call.
- The dial's four steps per detent follow the usual encoder layout. They were
  not measured on this unit.
- Audio starts about 6 s after boot: 3 s for the XMOS, then 2.5 s for the DAC.
