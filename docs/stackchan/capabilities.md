# M5StackChan (K151) capabilities

Research notes for porting claude-pet to the M5StackChan AI Desktop Robot
(SKU K151, CoreS3 core). Compiled 2026-09-05 from two research passes
(hardware, software) plus bench checks on the unit on this desk. Every
external claim carries a numbered source in its section. UNKNOWN means no
source was found, not a guess.

Companion index of the vendored repos: [repos.md](repos.md).

## Bench findings (this unit, 2026-09-05)

- Enumerates as Espressif `USB JTAG_serial debug unit`, VID 0x303a PID 0x1001,
  serial `80:45:6B:54:7F:44`, port `/dev/cu.usbmodem3101`. Same peripheral as the
  Freenove pet, so the bridge's serial transport applies unchanged.
- Factory firmware is on the unit, paired with the StackChan World app and on WiFi.
  It prints one ESP-IDF log line every 10 s and nothing else:
  `I (276141) SystemInfo: free sram: 29891 minimal sram: 19399`
  It ignores input (`\r\n`, `help\r\n` produced no reply). No serial console.
- **Opening the CDC port does not reset the sketch.** The log timestamp kept
  counting across two separate port opens (246141 -> 266141 -> 276141 ms). This
  settles the hardware report's "UNVERIFIED" item below.
- Factory firmware has ~30 KB free SRAM at idle. Our Arduino build replaces it.

## Factory flash backup (2026-09-05)

`firmware/build-archive/stackchan-factory-20260905.bin` — full 16 MB dump,
sha256 `a9f9f716…`, gitignored (the archive dir ignores everything). Read with
the esptool bundled in the Arduino core at the default baud. Passing
`-b 460800` stalled at 110 KB with "Serial data stream stopped" on the
USB-Serial/JTAG link, so do not pass `-b` on this board.

Partition table decoded from the dump matches `vendor/stackchan/StackChan/firmware/partitions.csv`:
`nvs` 0x9000, `otadata` 0xd000, `phy_init` 0xf000, `ota_0` 0x20000, `ota_1`
0x510000 (both 0x4f0000), `assets` 0xa00000 (4 MB), `coredump` 0xe00000.
The unit boots from `ota_1` (`ota confirm check: partition=ota_1`), so it has
taken at least one OTA since the factory image. 43.5% of the flash is 0xFF.

Restore: `esptool -p /dev/cu.usbmodem3101 write-flash 0x0 firmware/build-archive/stackchan-factory-20260905.bin`
(also restores NVS, so WiFi credentials, app pairing, and servo zero offsets come back).

## What this means for the claude-pet port

1. **Transport:** USB serial NDJSON, unchanged. Same `data.h` parser, same daemon.
   The daemon's port glob must match `/dev/cu.usbmodem*` (the eink install
   currently points at `usbserial-*`).
2. **Display and touch:** M5Unified `M5.Display` (M5GFX, LovyanGFX API) and
   `M5.Touch` replace the TFT_eSPI + FT6336 compat layer. 320x240 landscape.
3. **Body:** StackChan-BSP `M5StackChan.Motion` for the head (Feetech serial
   servos on UART1, G6/G7, 1 Mbaud), `setRgbColor`/`refreshRgb` for the 12 LEDs
   through the PY32 expander, `TouchSensor` for the three top zones.
4. **Backup first:** dump the full 16 MB flash with esptool before the first
   custom flash. Restore path is M5Burner (`StackChan-UserDemo`) or the dump.
5. **Prior art to read:** `stackchan-mcp` (Claude Code -> gateway -> ESP32) and
   `stackchan-nukoevi` (custom firmware for a Claude Code Channels setup).

---

# Part A — hardware


Date: 2026-09-05. Sources are numbered in `## Sources`. `[n]` after a claim points at that list.

## Body interface

The body is not a USB or single-bus peripheral. It hangs off the CoreS3 M-Bus (2x15 header) as a mix of direct ESP32-S3 GPIO, a shared I2C bus, and a sub-MCU IO expander. [1][2][7]

Direct GPIO from the ESP32-S3 (official pin map) [1]:

| Signal | ESP32-S3 pin | Notes |
|---|---|---|
| Servo_TX | G6 | UART1 TX, 1 Mbps, to Feetech SCS half-duplex bus [3] |
| Servo_RX | G7 | UART1 RX [3] |
| IR_SEND | G5 | drives an S8050 transistor + IR LED on the base power board [8] |
| IR_REC | G10 | IRM-56384 receiver on the top touch board, via the 8-pin FPC [11] |
| I2C_SCL | G11 | same bus as the CoreS3 internal I2C (`M5.In_I2C`) [1][2] |
| I2C_SDA | G12 | |

Body I2C devices (7-bit) [1]:

| Device | Address | Board |
|---|---|---|
| PY32L020 IO expander (sub-MCU) | 0x6F default, 0x71 when ADD_SEL high | base power board (U3) [8] |
| INA226 battery monitor | 0x41 | base power board, 10 mOhm shunt on BAT+ [8] |
| Si12T 3-zone touch (TSM12 compatible) | 0x68 | top touch board [11] |
| ST25R3916 NFC | 0x50 | top touch board [11] |

Sub-MCU: the PY32L020 runs M5Stack's IO-expander firmware. It is the only path to servo power and to the LEDs. Register map from the BSP driver [4]:

```
0x00/0x01 UID_L/H        0x02 VERSION (begin() fails if 0 or 0xFF)
0x03/0x04 GPIO_M_L/H     direction, 1 = output
0x05/0x06 GPIO_O_L/H     output level
0x07/0x08 GPIO_I_L/H     input level
0x09/0x0A GPIO_PU_L/H    pull-up       0x0B/0x0C GPIO_PD_L/H pull-down
0x0D/0x0E GPIO_IE_L/H    irq enable    0x0F/0x10 GPIO_IT_L/H irq type
0x11/0x12 GPIO_IS_L/H    irq status (write 1 to clear)
0x13/0x14 GPIO_DRV_L/H   1 = open-drain
0x15 ADC_CTRL [7]=busy [6]=start [2:0]=channel 1..4   0x16/0x17 ADC_D_L/H
0x1B..0x22 PWM1..4 duty L/H (12-bit, H[7]=enable, H[6]=polarity)
0x24 LED_CFG   [5:0]=LED count (max 32), bit6 = refresh strobe
0x25/0x26 PWM_FREQ_L/H
0x30.. LED_RAM  2 bytes per LED, RGB565 little-endian
```

Driver pin index is 0-based. Pin index 0 = PY32 `IO1` = `VM_EN` (servo 5 V rail enable). Pin index 13 = PY32 `IO14` = `RGB` (WS2812C data). The BSP sets both as push-pull outputs with pull-up, then `setLedCount(12)`. [1][3][4][8]

WS2812C x12 (4020 package) sit on the top touch board on the `BUS_5V` rail. LED index 0-5 = left row, 6-11 = right row. You do not bit-bang them from the ESP32. You write RGB565 into LED_RAM and strobe bit6 of 0x24. [3][4][11]

IR: `IRremoteESP8266` on G5 (send) and G10 (receive). No expander involvement. [5]

Touch: Si12T output register 0x10 packs three 2-bit intensity values (0 = idle, 1-3 = pressure). BSP polls over I2C, remaps index so 0/1/2 = Front/Middle/Back. `Touch_IRQ` exists on the FPC but where it terminates is UNKNOWN. Sensitivity registers 0x02-0x07, CFIG 0x08, CTRL 0x09, Ref_rst 0x0A/0x0B. [6][12]

NFC: `M5Unit-NFC` (`m5::unit::UnitNFC` on `M5.In_I2C`). NFC IRQ routing is UNKNOWN. [5]

Base USB-C data path: the base ring board carries USB_5V/D+/D- to the adapter board, then to the power board `CON1` (1.25 mm 4-pin: USB_5V, GND, D+, D-, 51 R series). The CoreS3 board has an internal 4-pin `J6` (VUSB, GND, USB_D_N, USB_D_P). The kit ships a "1.25-4P male-to-female straight extension cable (100mm)". The M-Bus itself carries no USB data. [1][8][9][10]

Base Grove ports (from the base, not the CoreS3 side): PORT.A G2/G1, PORT.B G9/G8, PORT.C G17/G18. [1]

## Servos

Two Feetech SCS0009 TTL serial servos on one half-duplex bus. Servo_TX (G6) and Servo_RX (G7) are merged onto `Servo_SIG` on the power board and reach the servos through 3-pin `HC-5264-3A` headers (Servo_SIG, VM, GND) on the adapter board. [3][8][9]

Datasheet facts: 4.8-6 V, 38400 bps-1 Mbps (default 1 Mbps), ID 0-253, 300 deg over 0-1024, 0.293 deg/step, neutral 511, feedback = position/load/voltage/temperature, max position update 1 ms. [13]

BSP bring-up (`servo_init`) [3]:

```cpp
_scs_bus.begin(UART_NUM_1, 1000000, 6, 7);   // SCSCL class, 8N1
// yaw   : id 1, defaultZeroPos 460, angleLimit -1280..1280 (0.1 deg units), rawPosLimit 0..1000, PWM mode allowed
// pitch : id 2, defaultZeroPos 620, angleLimit 0..900,      rawPosLimit 0..1000
```

Angle to raw: `raw = zero + angle_tenths * 16 / 5 / 10` (0.3125 deg per step in the BSP). Raw to angle: `(raw - zero) * 5 * 10 / 16`. Zero positions persist in NVS namespace `servo`, keys `zero_pos_1` / `zero_pos_2`. [3]

Servo power: `M5StackChan.setServoPowerEnabled(bool)` writes PY32 pin 0 (`VM_EN`). `VM` is a 5 V rail from a TPS61088 boost fed by BAT+. No VM, no motion. [3][8]

Register-level commands (SCSCL, Feetech SCS memory map) [3][14]:

| Action | Call | Register |
|---|---|---|
| Move to raw position | `WritePos(id, pos, time=20, speed=0)` | 42/43 GOAL_POSITION, 44/45 GOAL_TIME, 46/47 GOAL_SPEED |
| Read position | `ReadPos(id)` (returns -1 on fail) | 56/57 PRESENT_POSITION |
| Moving flag | `ReadMove(id)` | 66 MOVING |
| Torque on/off | `EnableTorque(id, 0/1)`; `ReadToqueEnable(id)` | 40 TORQUE_ENABLE |
| Continuous rotation (yaw only) | `SwitchMode(id, 1)` then `WritePWM(id, -1023..1023)` | mode 1 zeroes ANGLE_LIMIT 9-12, PWM written to 44/45, bit10 = direction |
| Back to position mode | `SwitchMode(id, 0)` restores cached angle limits | 9-12 |
| Also readable | `ReadSpeed`, `ReadLoad`, `ReadVoltage`, `ReadTemper`, `ReadCurrent` | 58-70 |

High-level API (`M5StackChan.Motion`, units 0.1 deg, speed 0-1000) [3][5]:
`moveX/moveYaw(angle, speed)`, `moveY/movePitch`, `move(x, y, speed)`, `goHome`, `stop`, `rotateX(velocity -1000..1000)` (negative = CW), `lookAtNormalized(x, y)`, `lookAtPoint(x, y, z)`, `getCurrentXAngle()`, `getCurrentYAngle()`, `isMoving()`, `setTorqueEnabled`, `setAutoTorqueReleaseEnabled`, `setAutoAngleSyncEnabled`, `setCurrentPostionAsHome()`. `Motion.init` spawns a FreeRTOS task that runs a spring animation at 50 Hz and releases torque when at rest. Yaw range in the BSP is -128..+128 deg in position mode. Pitch range 0..90 deg. M5Stack advises 5-85 deg on the Y axis. [1][3]

Feedback: `ScsServo::getCurrentAngle()` calls `ReadPos`, validates against rawPosLimit, converts with the zero offset, clamps to angleLimit. A failed read falls back to the commanded angle. [3]

## CoreS3 peripherals

All I2C devices below share `I2C_SYS` on G12 (SDA) / G11 (SCL). `M5Unified` detects the board as `board_M5StackChan` when it finds AXP2101 + AW9523 + GC0308 and PY32 firmware version >= 4 at 0x6F. [2][15][16]

| Subsystem | Chip | Pins / bus | Arduino library |
|---|---|---|---|
| LCD 320x240 | ILI9342C | SPI: MOSI G37, SCK G36, CS G3, DC G35; RST = AW9523 P1_1; backlight = AXP2101 DLDO1 | M5GFX via `M5.Display` [2] |
| Touch | FT6336U 0x38 | I2C; RST = AW9523 P0_0; INT = AW9523 P1_2 -> I2C_INT G21 | M5Unified `M5.Touch` [2][15] |
| Camera | GC0308 0x21 | D0-D7 = G39,G40,G41,G42,G15,G16,G48,G47; VSYNC G46; HREF G38; PCLK G45; XCLK external 20 MHz; RST = AW9523 P1_0 | esp32-camera / `M5CoreS3` camera example [2][17] |
| Mic codec | ES7210 0x40 | I2S: BCK G34, WS G33, DATI G14, MCLK G0 | M5Unified `M5.Mic` [2][15] |
| Speaker amp | AW88298 0x36 | I2S: BCK G34, WS G33, DATO G13; RST = AW9523 P0_2; INT = P1_3 | M5Unified `M5.Speaker` [2][15] |
| Light / proximity | LTR-553ALS-WA 0x23 | I2C, shares camera ribbon | no M5Unified class; use a generic LTR-553 driver [2] |
| IMU | BMI270 0x69 | I2C | M5Unified `M5.Imu` [2] |
| Magnetometer | BMM150 0x10 | on BMI270 aux I2C (sensor hub) | M5Unified `M5.Imu` [2] |
| PMIC | AXP2101 0x34 | I2C; ALDO1 1.8 V AW88298, ALDO2 3.3 V ES7210, ALDO3 3.3 V camera, ALDO4 3.3 V TF | M5Unified `M5.Power` [2][16] |
| RTC | BM8563 0x51 | I2C; IRQ -> AXP2101 wakeup | M5Unified `M5.Rtc` [2] |
| IO expander | AW9523B 0x58 | I2C; P0_1 BUS_OUT_EN, P0_5 USB_OTG_EN, P1_7 BOOST_EN (SY7088) | M5Unified internal [2][10][16] |
| microSD | - | SPI: MISO G35, MOSI G37, SCK G36, CS G4; 16 GB max | `SD.h` [2] |
| Grove (CoreS3 side) | HY2.0-4P | A: G2 SDA/G1 SCL; B: G9/G8; C: G17/G18 | - [2] |
| Buttons | - | Power button -> AXP2101 (`M5.BtnPWR`); RST is hardware reset | M5Unified [2] |
| M-Bus | 2x15 | pin 28 = BUS_OUT 5 V, pin 30 = VBAT, pin 22 = ESP_BOOT (G0), 13/14 = G44/G43 UART0, 21/22 = G6/G7 | - [2][10] |

Arduino board: select "M5CoreS3". Board definition sets `usb_mode=1` (hardware CDC + JTAG) and `cdc_on_boot=1`. Library: `StackChan-BSP` 1.1.0 (`#include <M5StackChan.h>`), depends on `M5Unified`, `IRremoteESP8266`, `M5Unit-NFC`. `M5StackChan.begin()` calls `M5.begin()` first. [3][18][19]

## USB

VID/PID: `pins_arduino.h` for `m5stack_cores3` defines `USB_VID 0x303a`, `USB_PID 0x1001`. esptool treats PID 0x1001 as the USB-JTAG-Serial peripheral (`USB_JTAG_SERIAL_PID = 0x1001`). D- = GPIO19, D+ = GPIO20. [18][20][21][22]

Two USB-C ports carry data: CoreS3 side and base. M5Stack recommends the base port "to avoid accidents caused by motor movement". [1]

Opening the CDC port: no source states that a plain open resets the chip. The Arduino `HWCDC` driver has no DTR/RTS handling. esptool resets only through a specific DTR/RTS pattern (`USBJTAGSerialReset`: DTR high with RTS low, then RTS high with DTR low, then both low). Verified on the bench 2026-09-05: a plain port open does NOT reset the sketch (see Bench findings above). [20][21]

Bootloader / download mode: hold RST for about 3 s (the Arduino guide says about 2 s) until the green LED next to the RST button turns on, release, LED turns off. The CoreS3 "self-built delay circuit" pulls the boot strap. [1][2][19] esptool can also enter it with `--before usb-reset`, and `--after watchdog-reset` re-enumerates the port (e.g. `/dev/ttyACM0` -> `/dev/ttyACM1`). [21] The device disappears from the host while in reset, on Deep-sleep, and if firmware reconfigures GPIO19/20. [22]

## Power

Body battery: 550 mAh, on the base adapter board with an FH9261 battery-protection circuit. BAT+ passes a 10 mOhm shunt (INA226) on the power board and reaches M-Bus pin 30 (`VBAT`). `VBAT` is the AXP2101 battery node on the CoreS3. So yes: the body battery feeds the CoreS3 PMIC directly and is charged by it. `M5StackChan.getBatteryVoltage()` / `getBatteryCurrent()` read the INA226 (positive = discharging). The factory firmware sets AXP2101 charge current to 700 mA. [3][8][9][10][23]

Whether the StackChan CoreS3 also keeps its own 500 mAh cell (`BAT1/NC` on the CoreS3 schematic) is UNKNOWN. The K151 spec sheet lists only the 550 mAh body battery. [1][10]

Only CoreS3 USB-C plugged: 5 V enters the AXP2101 `VBUS`. `BUS_OUT` (M-Bus pin 28 -> body `BUS_5V`, which powers the WS2812C, Si12T, IR receiver 3.3 V LDO) is switched by AW9523 `BUS_OUT_EN`. `M5Unified::begin()` calls `Power.setExtOutput(cfg.output_power)` with `output_power = true`, so BUS_5V is on unless `!getBatState() && TSVoltage > 2.0 && isVBUS()` (then it logs "setExtPower(true) is canceled"). The servo rail `VM` is a separate TPS61088 boost from BAT+ gated by PY32 `VM_EN`. It does not come from BUS_5V. With the body battery attached and PMIC charging, servos work from either port. With no battery, servo behavior is UNKNOWN. [8][10][16]

Only base USB-C plugged: USB_5V from the ring board reaches the CoreS3 through the 4-pin `J6` cable (`VUSB`). Docs confirm both ports power the unit ("try supplying power through each of the two USB-C ports separately"). [1][8][10]

Power on: short press the power button (left side). Off: hold 6 s. Reset: RST button. [1]

## Sources

1. https://docs.m5stack.com/en/stackchan (product page, pin map, I2C table, download mode, power on/off)
2. https://docs.m5stack.com/en/core/CoreS3 (CoreS3 pin map, I2C table, M-Bus, power management)
3. https://github.com/m5stack/StackChan-BSP/blob/main/src/M5StackChan.cpp
4. https://github.com/m5stack/StackChan-BSP/blob/main/src/drivers/PY32IOExpander/PY32IOExpander.cpp
5. https://github.com/m5stack/StackChan-BSP/tree/main/examples (IR/Send, IR/Receive, NFC/Detect, RGB_LED, Servo/BasicMovement, TouchSensor)
6. https://github.com/m5stack/StackChan-BSP/blob/main/src/drivers/Si12T/Si12T.h
7. https://github.com/m5stack/StackChan (firmware/main/hal/board/config.h)
8. https://m5stack-doc.oss-cn-shenzhen.aliyuncs.com/1205/SCH_Power.pdf
9. https://m5stack-doc.oss-cn-shenzhen.aliyuncs.com/1205/SCH_Adapter.pdf
10. https://m5stack-doc.oss-cn-shenzhen.aliyuncs.com/490/Sch_M5_CoreS3_v1.0.pdf
11. https://m5stack-doc.oss-cn-shenzhen.aliyuncs.com/1205/SCH_Touch.pdf
12. https://github.com/m5stack/StackChan-BSP/blob/main/src/utils/touch_sensor/touch_sensor.cpp
13. https://m5stack-doc.oss-cn-shenzhen.aliyuncs.com/1205/SCS0009.pdf
14. https://github.com/m5stack/StackChan-BSP/blob/main/src/drivers/FTServo_Arduino/src/SCSCL.h
15. https://github.com/m5stack/M5GFX/blob/master/src/M5GFX.cpp (board autodetect, PY32 version check at 0x6F)
16. https://github.com/m5stack/M5Unified/blob/master/src/utility/Power_Class.cpp
17. https://github.com/m5stack/StackChan/blob/main/firmware/main/hal/board/stackchan.cc
18. https://github.com/espressif/arduino-esp32/blob/master/variants/m5stack_cores3/pins_arduino.h
19. https://docs.m5stack.com/en/arduino/stackchan/program
20. https://github.com/espressif/arduino-esp32/blob/master/cores/esp32/HWCDC.cpp
21. https://github.com/espressif/esptool/blob/v4.8.1/esptool/reset.py and https://docs.espressif.com/projects/esptool/en/latest/esp32s3/esptool/advanced-options.html
22. https://docs.espressif.com/projects/esp-idf/en/latest/esp32s3/api-guides/usb-serial-jtag-console.html
23. https://m5stack-doc.oss-cn-shenzhen.aliyuncs.com/1205/SCH_Ring.pdf
24. https://github.com/espressif/arduino-esp32/blob/master/boards.txt (m5stack_cores3 usb_mode / cdc_on_boot)
25. https://m5stack-doc.oss-cn-shenzhen.aliyuncs.com/1205/PY32L020_Datasheet_EN.pdf (not read; listed for the port)

## Unknowns

- Where `Touch_IRQ` (Si12T) and the ST25R3916 IRQ terminate. Not visible in the text layer of the schematics. BSP polls both over I2C.
- ~~Whether a plain CDC port open resets the sketch.~~ Resolved on the bench: it does not.
- Whether the StackChan CoreS3 retains its internal 500 mAh cell in parallel with the 550 mAh body cell.
- Servo behavior with no body battery and USB only (VM boost is fed from BAT+).
- PY32L020 ADC channel and PWM channel pin assignments on this board (driver exposes them, schematic text does not name them).
- PY32 firmware protocol for reading LED RAM back, and the meaning of VERSION values other than ">= 4".
- Exact CON4 / FPC1 pin order on the power board beyond the nets named above.

---

# Part B — software, firmware, app


Date: 2026-09-05. Repo metadata from `gh api`, library versions from `arduino-cli lib search` (arduino-cli 1.5.1, esp32 core 3.3.10 installed).

## Repositories

| repo | contents | stack | license | last push | targets K151? |
|---|---|---|---|---|---|
| github.com/m5stack/StackChan [1] | `firmware/` (factory device firmware), `remote/` (ESP-NOW joystick remote), `app/` (StackChan World mobile app), `server/` (backend). README: "Update of this repo could be a little late than the released firmware and mobile app." | firmware: ESP-IDF v5.5.4, C/C++, vendors `78/xiaozhi-esp32` v2.2.4 + patch, `mooncake` v2.3.3, `esp-now` [2]. app: Flutter/Dart [3]. server: Go + MySQL [4]. | repo root: none reported by GitHub. `firmware/LICENSE`, `server/LICENSE`: MIT, M5Stack 2026. app README badge: MIT. | 2026-08-19 | Yes. Commercial K151 only. |
| github.com/m5stack/StackChan-BSP [5] | Arduino board-support library: `M5StackChan.h`, drivers `FTServo_Arduino`, `PY32IOExpander`, `Si12T`; examples `INA226 IR NFC RGB_LED Servo TouchSensor`. | Arduino C++. depends `M5Unified,IRremoteESP8266,M5Unit-NFC`. | MIT | 2026-08-28; release 1.1.0 2026-05-12 | Yes. K151 body (Feetech serial servos, LEDs, touch, NFC). |
| github.com/stack-chan/stack-chan [6] | Community firmware (`firmware/host`, `mods`), browser tools (web flasher, BLE preferences, Blockly MOD editor, simulator), `case/`, `schematics/`, docs. | Moddable SDK JavaScript/TypeScript on ESP-IDF; MODs in JS/TS. | Apache-2.0 | 2026-09-05; release v1.1.0 2026-08-25 | Yes. "M5StackChan CoreS3 is the standard configuration and the target used for physical-device release validation." Also M5Stack Basic, Core2, CoreS3 DIY builds. |
| github.com/stack-chan/m5stack-avatar [7] | Face-rendering library `Avatar` class (`avatar.init()`, expressions, mouth open ratio). Same repo as `meganetaaan/m5stack-avatar` (both report identical stars and push date). | Arduino/PlatformIO C++, depends M5Unified. | MIT | 2024-09-13; tag v0.10.0 | Hardware-agnostic display library. Runs on CoreS3. No K151 body support. |
| github.com/mongonta0716/stack-chan-tester [8] | PWM servo test/offset-calibration app for DIY kits (M5GoBottom, Takao). | PlatformIO Arduino C++, deps `M5Stack-Avatar@0.10.0`, `stackchan-arduino`. | MIT | 2026-05-07 | No. "This is only for ArduinoFramework and PWM servos." CoreS3 env uses Port.C G18/G17 PWM. |
| github.com/stack-chan/stackchan-arduino [9] | Config loader + servo abstraction for DIY builds: PWM (SG90), SCS (Feetech SCS0009), Dynamixel XL330. Arduino index name `stackchan-arduino` 0.0.8. | Arduino C++ | MIT | 2026-07-29 | No. Aimed at DIY kits. UNKNOWN whether its SCS mode drives the K151 bus. |

Notes:
- `mongonta0716/m5stack-avatar` and `mongonta0716/stackchan-arduino` are forks, not primary repos.
- The Arduino Library Manager entry for the BSP is named `M5StackChan` (latest 1.0.1 in the index), while the GitHub `library.properties` says 1.1.0. Index lags GitHub.

## Factory firmware and restore

Features (product page and README) [1][10]: AI Agent with wake word "Hi, StackChan", animations, ESP-NOW remote control (receiver/sender, channel and receiver ID settings), online app downloads (App Store), OTA. OTA runs when entering AI Agent mode: "the device checks for firmware updates online, then automatically downloads, installs, and restarts." [11]

Firmware apps in source (`firmware/main/apps/`): `app_ai_agent app_app_center app_avatar app_dance app_espnow_ctrl app_ezdata app_launcher app_setup`. HAL includes `hal_ble.cpp`, `hal_espnow.cpp`, `hal_ota.cpp`, `hal_mcp.cpp`, `hal_ws_avatar.cpp`, `hal_servo.cpp` [1]. The AI Agent is xiaozhi-esp32 based and exposes MCP tools `self.robot.get_head_angles`, `self.robot.set_head_angles`, `self.robot.set_led_color`, `self.robot.create_reminder` to the cloud agent (`hal_mcp.cpp`).

Partition table (`firmware/partitions.csv`) [1]: `ota_0`/`ota_1` app slots 0x4f0000 each, `assets` spiffs 4M at 0xA00000, `coredump`. This is the layout an Arduino build will overwrite.

Servo bus: `hal_servo.cpp` uses Feetech `SCSCL` on `UART_NUM_1`, 1,000,000 baud, pins 6/7 (`_scs_bus.begin(UART_NUM_1, 1000000, 6, 7)`) [1]. A community report says servo zero position is "stored on the servo itself" and survives reflash [12].

Published where: M5Burner, search "StackChan" with "Only Official" enabled [11]. Community report names the entry `StackChan-UserDemo` (author M5Stack) with versions V1.2.4 (shipped), V1.4.3, V1.4.4 [12]. Docs release history lists V1.4.3 2026-07-02 and V1.4.4 2026-07-13 [13]. Exact M5Burner entry name not confirmed on docs.m5stack.com: treat `StackChan-UserDemo` as community-sourced.

Source public: yes, `m5stack/StackChan/firmware` builds with ESP-IDF v5.5.4 (`python3 ./fetch_repos.py`, `idf.py build`, `idf.py flash`) [2]. A community build of v1.4.1 booted but showed app pairing errors and a non-working Avatar mode, so the repo build is not byte-equivalent to the M5Burner release [14].

"User firmware" the app requires: docs say the app needs firmware new enough for the pairing handshake; a unit shipped on V1.2.4 failed pairing until updated to V1.4.4 [12]. Docs text distinguishes "factory firmware" from "custom firmware ... using Arduino, UiFlow, or other methods" [11]. No separate "user firmware" artifact beyond the M5Burner `StackChan-UserDemo` entry is documented. UNKNOWN whether the name "UserDemo" implies a different build from the preinstalled image.

Backup (esptool, ESP32-S3 docs) [15]: `esptool flash-id` to confirm 16MB, then `esptool -p PORT -b 460800 read-flash 0 ALL flash_contents.bin`. Restore: `esptool -p PORT write-flash 0x0 flash_contents.bin`. Current esptool spells commands with hyphens; older releases used `read_flash`/`write_flash`. Download mode on K151: hold reset about 2 s until the internal green LED lights [16].

Restore via M5Burner: install M5Burner [17], search "StackChan", enable "Only Official", download, burn [11]. stack-chan/stack-chan README confirms this path: "Flashing this repository's firmware replaces the factory firmware supplied by M5Stack. To restore it, follow the restore procedure in the M5Stack product documentation and use M5Burner." [6] Backup of NVS/servo calibration is not documented by M5Stack. The community esptool route writes the M5Burner `.bin` at offset 0x0 [12].

## StackChan World app

What it does [3][10]: BLE device binding (`flutter_blue_plus`), AI Agent configuration (name, language, model, voice, personality, memory), Wi-Fi provisioning (2.4 GHz only), remote avatar control, video monitoring, motion choreography, dance mode. Requires an M5Stack account.

Transport: cloud. App and device both connect to the Go server at `/stackChan/ws`; "The server forwards audio, image, motion-control, call-state, and status messages." [4] Device WebSocket URL in firmware: `{server}/stackChan/ws?deviceType=StackChan` (`hal_ws_avatar.cpp`) [1]. Device REST auth uses an RSA-OAEP-encrypted `mac|nonce|timestamp` token [4]. App base URL is a compile-time constant in `app/lib/network/urls.dart` [3].

Local API: BLE GATT only. Factory firmware `gatt_svr.c` defines a 128-bit StackChan service with characteristics `motion`, `avatar`, `config`, `rgb` (JSON payloads, fragmented notify), plus standard Battery Service 0x180F/0x2A19 [1]. UUID values and JSON schemas are in `firmware/main/hal/utils/bleprph/gatt_svr.c` and `hal_ble.cpp`; not documented outside source. No local HTTP, WebSocket, or mDNS API on the factory firmware: UNKNOWN beyond the BLE GATT found in source.

## WiFi capabilities

Capability note only. arduino-esp32 3.3.10 ships `WiFi`, `ESP_NOW`, `ESPmDNS`, `ArduinoOTA`, `Update`, `HTTPUpdate`, `WebServer`, `Network`, `NetworkClientSecure`, `WiFiProv` in `libraries/` [18]; Wi-Fi and ESP-NOW have API pages in the Espressif docs [19]. CoreS3 has Wi-Fi and BLE (ESP32-S3) [5]. Factory firmware uses `esp-now` component and ESP-IDF OTA; StackChan-BSP has no networking code. Our transport stays USB serial.

## Arduino toolchain and libraries

FQBN `esp32:esp32:m5stack_cores3` (esp32 core 3.3.10, `arduino-cli board details`) defaults:
- `USBMode=hwcdc` (Hardware CDC and JTAG), alt `default` (USB-OTG TinyUSB)
- `CDCOnBoot=cdc` (Enabled), alt `default`
- `UploadMode=default` (UART0 / Hardware CDC), alt `cdc`
- `FlashSize=16M` (alt `32M`), `FlashMode=qio`, `PSRAM=enabled` (QSPI), alt `opi`, `disabled`
- `PartitionScheme=app3M_fat9M_16MB` (3MB APP / 9.9MB FATFS) default; other 16MB options `fatflash` (2MB APP/12.5MB FATFS), `esp_sr_16`, `factory_4apps`, `factory_6apps`, `custom`; smaller schemes `default`, `min_spiffs`, `huge_app`, `no_ota`
- `CPUFreq=240`, `DebugLevel=none`, `EraseFlash=none`, `JTAGAdapter=default`

Libraries (Arduino index, `arduino-cli lib search`, 2026-09-05):
| lib | version | provides |
|---|---|---|
| M5Unified | 0.2.21 (GitHub tag 0.2.21) | Board detection and unified API: `M5.Display` (`M5GFX`, alias `M5.Lcd`), `M5.Displays(i)`, `M5.Touch` (`Touch_Class`), `M5.BtnA/BtnB/BtnC/BtnEXT/BtnPWR` (`Button_Class`), `M5.Speaker`, `M5.Mic`, `M5.Imu`, `M5.Rtc`, `M5.Power`, `M5.Log`, `M5.In_I2C`, `M5.Ex_I2C` (`src/M5Unified.hpp`) [20]. README lists "M5StackChan" under supported ESP32-S3 devices [21]. |
| M5GFX | 0.2.28 | LovyanGFX-based display driver; `LGFX_Sprite` off-screen canvases; M5Unified depends on it [22]. |
| M5Stack_Avatar | 0.10.0 | `Avatar` face renderer over M5Unified [7]. |
| M5CoreS3 | 1.0.1 (pushed 2026-07-28) | Legacy CoreS3 library; depends `M5GFX,M5Unified,M5Family` [23]. Not needed when using M5Unified. |
| M5StackChan (StackChan-BSP) | 1.0.1 index, 1.1.0 GitHub | `M5StackChan.begin()/update()`, `M5StackChan.Motion` (`moveYaw`, `movePitch`, `move`, `goHome`, `rotateYaw`, `lookAtNormalized`, `lookAtPoint`, `setTorqueEnabled`, `setCurrentPostionAsHome`), `TouchSensor`, `setServoPowerEnabled`, `setRgbColor`, `refreshRgb`, `showRgbColor`, `getBatteryVoltage`, `getBatteryCurrent` (`src/M5StackChan.h`, `src/utils/motion/motion.h`) [5]. |

Servos: M5Unified does not drive the K151 body servos. M5Unified's StackChan entry covers board detection; servo, LED, touch, INA226 and NFC come from StackChan-BSP (`FTServo_Arduino` driver, Feetech serial bus) [5]. Arduino docs instruct installing the "M5StackChan driver library" with its dependencies and use the `RGB LED` example [16].

## UiFlow2

UiFlow2 needs the StackChan UiFlow2 firmware burned from M5Burner [24]. MicroPython API: `from hardware.stackchan import StackChan, SERVO_ID_X, SERVO_ID_Y`; methods `set_servo_zero`, `set_servo_power`, `set_servo_torque`, `set_servo_angle(servo_id, angle_deg, time_ms=10, speed=0)`, `get_servo_angle`, `set_servo_x_pwm`, `set_rgb_color`, `get_rgb_color`, `get_touch`, `get_battery_voltage/current/power`, `nfc` attribute [25]. Block categories: Servo Control, RGB Strip, Touch Pad, Battery/Power [25]. No avatar or ESP-NOW blocks are listed on that page: UNKNOWN whether avatar blocks exist elsewhere in UiFlow2.

## Sources

1. https://github.com/m5stack/StackChan
2. https://github.com/m5stack/StackChan/blob/main/firmware/README.md and firmware/repos.json
3. https://github.com/m5stack/StackChan/blob/main/app/README.md and app/pubspec.yaml, app/lib/network/urls.dart
4. https://github.com/m5stack/StackChan/blob/main/server/README.MD
5. https://github.com/m5stack/StackChan-BSP
6. https://github.com/stack-chan/stack-chan
7. https://github.com/stack-chan/m5stack-avatar
8. https://github.com/mongonta0716/stack-chan-tester
9. https://github.com/stack-chan/stackchan-arduino
10. https://docs.m5stack.com/en/products/sku/K151
11. https://docs.m5stack.com/en/StackChan
12. https://zenn.dev/shogaku/articles/stackchan-pairing-firmware?locale=en
13. https://docs.m5stack.com/en/history
14. https://note.com/aoya_uta/n/nf78cfadc9ac7?hl=en
15. https://docs.espressif.com/projects/esptool/en/latest/esp32s3/esptool/basic-commands.html
16. https://docs.m5stack.com/en/arduino/stackchan/program
17. https://docs.m5stack.com/en/uiflow/m5burner/intro
18. https://github.com/espressif/arduino-esp32/tree/master/libraries
19. https://docs.espressif.com/projects/arduino-esp32/en/latest/libraries.html
20. https://github.com/m5stack/M5Unified/blob/master/src/M5Unified.hpp
21. https://github.com/m5stack/M5Unified
22. https://github.com/m5stack/M5GFX
23. https://github.com/m5stack/M5CoreS3
24. https://docs.m5stack.com/en/uiflow2/stackchan/program
25. https://uiflow-micropython.readthedocs.io/en/latest/controllers/stackchan.html

## Unknowns

- Exact M5Burner entry name: `StackChan-UserDemo` is community-sourced [12], not confirmed on docs.m5stack.com.
- Whether the preinstalled image equals the M5Burner "UserDemo" build, and whether the M5Burner bin includes NVS/assets partitions (community flashed it at 0x0, which suggests a merged image).
- Whether the GitHub firmware tree matches any shipped release; one community build of v1.4.1 misbehaved with the app [14].
- BLE GATT UUID values and JSON schemas: present in source only, no published spec.
- Any local HTTP/WebSocket/mDNS API on the factory firmware: none found.
- UiFlow2 avatar or ESP-NOW blocks for StackChan.
- Whether `stackchan-arduino` SCS mode drives the K151 servo bus (UART1, 1 Mbaud, pins 6/7).
- Repo-root license for `m5stack/StackChan` (GitHub reports none; subdirectories carry MIT).
