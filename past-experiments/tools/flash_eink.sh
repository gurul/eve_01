#!/usr/bin/env bash
# Compile + flash the CrowPanel 4.2" e-ink build (past-experiments/firmware/claude_pet_eink).
#
# Unlike the FNK build there is no daemon-owns-the-port dance to skip: the
# CH340 UART port is owned by whoever holds it — if the hwlog capture daemon
# or the bridge daemon has it open, we flash *through* `hwlog flash` (which
# pauses capture around the tool) or ask you to stop the bridge.
set -euo pipefail
cd "$(dirname "$0")/../.."

# UploadSpeed pinned to 460800: the CH340 drops out mid-write at 921600.
FQBN="esp32:esp32:esp32s3:FlashSize=8M,PartitionScheme=default_8MB,UploadSpeed=460800"
SKETCH=past-experiments/firmware/claude_pet_eink
BUILD="$SKETCH/build"
PLIST="$HOME/Library/LaunchAgents/com.github.eve-bridge.daemon.plist"

arduino-cli compile --fqbn "$FQBN" --build-path "$BUILD" "$SKETCH"

# archive the exact ELF so a later panic backtrace stays symbolizable
mkdir -p past-experiments/firmware/build-archive
cp "$BUILD/claude_pet_eink.ino.elf" \
   "past-experiments/firmware/build-archive/claude_pet_eink-$(date +%Y%m%d-%H%M%S).elf"

PORT="${1:-$(ls /dev/cu.usbserial-* 2>/dev/null | head -1)}"
[ -n "$PORT" ] || { echo "no /dev/cu.usbserial-* port found" >&2; exit 1; }

UPLOAD=(arduino-cli upload --fqbn "$FQBN" -p "$PORT" --input-dir "$BUILD" "$SKETCH")
if command -v hwlog >/dev/null 2>&1 && hwlog status >/dev/null 2>&1; then
  hwlog flash -- "${UPLOAD[@]}"
elif launchctl list 2>/dev/null | grep -q com.github.eve-bridge.daemon; then
  # the bridge daemon owns the port exclusively and KeepAlive respawns it —
  # boot it out for the flash, bring it back after (same dance as flash.sh)
  launchctl unload "$PLIST"
  trap 'launchctl load -w "$PLIST"' EXIT
  # wait for the daemon to actually release the port — its reader thread
  # holds the fd past the unload, and a fixed sleep raced it ([Errno 35])
  for _ in $(seq 1 20); do
    lsof "$PORT" >/dev/null 2>&1 || break
    sleep 1
  done
  "${UPLOAD[@]}"
else
  "${UPLOAD[@]}"
fi
