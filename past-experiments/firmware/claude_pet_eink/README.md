# claude_pet_eink — CrowPanel 4.2" E-Paper build

Event-driven Claude status display for the **Elecrow CrowPanel ESP32 4.2"
E-Paper HMI** (ESP32-S3-WROOM-1-N8R8, SSD1683, 400×300 B/W, CH340 UART on the
USB-C port) — portrait, purely functional, no pet. Speaks the same NDJSON
protocol as `../claude_pet`, so the bridge daemon drives it unchanged —
point it at `/dev/cu.usbserial-*`.

Build/flash: `past-experiments/tools/flash_eink.sh` (from the repository root) (arduino-cli, FQBN
`esp32:esp32:esp32s3:FlashSize=8M,PartitionScheme=default_8MB` — `Serial` is
UART0 through the CH340; USB CDC stays off).

## Vendored driver

`EPD.{h,cpp}`, `EPD_SPI.{h,cpp}`, `EPD_GUI.{h,cpp}`, `EPD_font.h` come from
Elecrow's official demo repo
([CrowPanel-ESP32-4.2-E-paper-HMI-Display-with-400-300](https://github.com/Elecrow-RD/CrowPanel-ESP32-4.2-E-paper-HMI-Display-with-400-300),
`example/arduino/Examples/4.2_partial_refresh/`). The repo ships **no license
file**; the files are board-vendor sample code kept here unmodified except:

- `EPD.cpp` — `EPD_ReadBusy()` gained an 8s timeout so a wedged panel can't
  hang `loop()` forever (the daemon would RTS-reset an `[alive]`-silent board).
- `EPD.cpp` — every display path now maintains the SSD1683's old-data RAM
  plane (0x26). The stock driver never wrote it, so partial refreshes diffed
  against stale frames and text from consecutive screens visibly overlapped;
  see DESIGN.md ("old-image plane fix") for the full autopsy.

Pin map (from the vendor demo, verified against the wiki): SCK=12 MOSI=11
RES=47 DC=46 CS=45 BUSY=48, panel power enable GPIO7 (HIGH=on). Buttons,
active-low with external pull-ups: MENU/HOME=2, EXIT=1, rotary up=6, down=4,
press=5.
