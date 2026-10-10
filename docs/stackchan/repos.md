# StackChan reference repos (shared context)

Shallow clones live under `vendor/stackchan/` (gitignored, like the other
`vendor/` upstreams). Re-clone with the loop at the bottom. Cloned 2026-09-05.

Target hardware for this project: **M5StackChan K151** (CoreS3 core, ESP32-S3,
Feetech serial-bus servos, 12x WS2812, Si12T touch, ST25R3916 NFC). Transport
stays **USB serial** (native USB-Serial/JTAG, `/dev/cu.usbmodem*`). WiFi and
BLE are capabilities, not the link.

## Core

| Path | Upstream | What it is | Stack | Targets K151 |
|---|---|---|---|---|
| `StackChan/` | m5stack/StackChan | Official product repo: factory firmware (`firmware/`), ESP-NOW remote (`remote/`), StackChan World app (`app/`, Flutter), cloud backend (`server/`, Go). | ESP-IDF 5.5.4, vendors xiaozhi-esp32 | yes |
| `stack-chan/` | stack-chan/stack-chan | Original community project by Shinya Ishikawa. Firmware, web flasher, BLE preference tool, simulator, case, schematics. CoreS3 is its release-validation target. | Moddable SDK JS/TS | yes (CoreS3 build) |

## Libraries and tools

| Path | Upstream | What it is | Stack | Targets K151 |
|---|---|---|---|---|
| `StackChan-BSP/` | m5stack/StackChan-BSP | Arduino board support for the K151 body: `M5StackChan.begin()/update()`, `Motion` (yaw/pitch, home, torque), `TouchSensor`, RGB, battery (INA226), NFC. Drivers: `FTServo_Arduino`, `PY32IOExpander`, `Si12T`. Depends on M5Unified, IRremoteESP8266, M5Unit-NFC. | Arduino C++ | yes |
| `m5stack-avatar/` | stack-chan/m5stack-avatar | `Avatar` face renderer (eyes, mouth, expressions) over M5Unified. Display only. | Arduino C++ | display only |
| `stackchan-arduino/` | stack-chan/stackchan-arduino | YAML config loader plus servo abstraction (PWM, Feetech SCS, Dynamixel) for DIY kits. | Arduino C++ | DIY. SCS mode on the K151 bus is UNKNOWN |
| `awesome-stack-chan/` | stack-chan/awesome-stack-chan | Curated list: hardware, parts, firmware, mods, articles. | Markdown | index |
| `RoboEyes/` | FluxGarage/RoboEyes | Animated robot eyes (moods, blink, idle, flicker, sweat) as a header-only template over any Adafruit-GFX-shaped display. Used for the StackChan face (adapter over an M5GFX 1-bit sprite). Linked into `~/Documents/Arduino/libraries/FluxGarage_RoboEyes`. | Arduino C++ | display only |

## Custom firmware and AI integrations

| Path | Upstream | What it is | Stack | Targets K151 |
|---|---|---|---|---|
| `stackchan-mcp/` | kisaragi-mochi/stackchan-mcp | MCP bridge: MCP client (Claude Code, Claude Desktop) → Python gateway (stdio) → ESP32 over WebSocket. Tools for head movement, camera capture, touch reads, avatar expression. **Closest prior art to this project.** | Python gateway + ESP32 firmware | yes |
| `stackchan-nukoevi/` | schroneko/stackchan-nukoevi | Personal custom firmware derived from the official tree: home screen, character animation, mic flow, MQTT text output, WebSocket TTS. Built for a local Claude Code Channels setup. Keeps `hal_servo.cpp`, `hal_mcp.cpp`, `gatt_svr.c`. | ESP-IDF | yes |
| `dotty-stackchan/` | BrettKinny/dotty-stackchan | Self-hosted voice stack: open firmware + xiaozhi-esp32-server + local coding agent. Local ASR/TTS. Marked unstable by its author. | ESP-IDF firmware + Python | yes (CoreS3) |
| `StackChan_Minimal/` | A-Uta/StackChan_Minimal | Minimal companion for **AtomS3R**: WiFi portal, whisper.cpp, OpenAI-compatible LLM, external TTS. | Arduino | no (AtomS3R) |
| `rt-net-stack-chan/` | rt-net/stack-chan | RT-net kit variant of the original: Moddable 4.9.5 pinned, Dynamixel XL330 servo, revised board. | Moddable SDK | no (RT kit) |
| `AI_StackChan_Ex/` | ronron-gh/AI_StackChan_Ex | Extended AI firmware (OpenAI, Realtime API, Module LLM). PlatformIO envs include `m5stack-cores3`. Bundles a copy of the BSP under `firmware/lib/M5StackChan`. | Arduino C++ | CoreS3 yes, K151 body via bundled BSP |

## Files worth opening first

| Question | File |
|---|---|
| How the factory firmware drives the servos | `StackChan/firmware/main/hal/hal_servo.cpp` (Feetech SCSCL, UART1, 1 Mbaud, pins 6/7) |
| Motion API the factory firmware uses | `StackChan/firmware/main/stackchan/motion/motion.h` |
| MCP tools the factory AI agent exposes (head angles, LED color, reminders) | `StackChan/firmware/main/hal/hal_mcp.cpp` |
| Local BLE GATT service (motion, avatar, config, rgb characteristics) | `StackChan/firmware/main/hal/utils/bleprph/gatt_svr.c` |
| Factory partition layout an Arduino build overwrites | `StackChan/firmware/partitions.csv` |
| Arduino API for the body | `StackChan-BSP/src/M5StackChan.h`, `StackChan-BSP/src/utils/motion/motion.h` |
| Three-zone touch driver | `StackChan-BSP/src/drivers/Si12T/Si12T.h` |
| Driving the kit from Claude Code today | `stackchan-mcp/README.md` |

## Re-clone

```bash
mkdir -p vendor/stackchan && cd vendor/stackchan
for r in m5stack/StackChan stack-chan/stack-chan m5stack/StackChan-BSP \
  stack-chan/m5stack-avatar stack-chan/stackchan-arduino stack-chan/awesome-stack-chan \
  kisaragi-mochi/stackchan-mcp schroneko/stackchan-nukoevi BrettKinny/dotty-stackchan \
  A-Uta/StackChan_Minimal rt-net/stack-chan ronron-gh/AI_StackChan_Ex FluxGarage/RoboEyes; do
  name=$(basename $r); [ "$r" = rt-net/stack-chan ] && name=rt-net-stack-chan
  git clone --depth 1 "https://github.com/$r.git" "$name"
done
```

See [capabilities.md](capabilities.md) for the hardware and software capability notes.
