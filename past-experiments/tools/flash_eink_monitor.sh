#!/usr/bin/env bash
# Compile + flash the CrowPanel 4.2" e-ink *monitor* variant
# (past-experiments/firmware/claude_pet_eink_monitor) — the read-only landscape agent board.
#
# Same flashing dance as flash_eink.sh: the CH340 UART port has a single
# owner, so we flash *through* `hwlog flash` when the capture daemon holds it,
# or boot the bridge daemon off the port for the duration.
#
# NOTE: this variant expects the bridge daemon to run with
# EVE_MONITOR_ONLY=1. Flashing it without setting that leaves every
# permission prompt waiting PERMISSION_WAIT_SECS on buttons this build does
# not read. See README "E-ink agent monitor".
set -euo pipefail
cd "$(dirname "$0")/../.."

# UploadSpeed pinned to 460800: the CH340 drops out mid-write at 921600.
FQBN="esp32:esp32:esp32s3:FlashSize=8M,PartitionScheme=default_8MB,UploadSpeed=460800"
SKETCH=past-experiments/firmware/claude_pet_eink_monitor
BUILD="$SKETCH/build"
PLIST="$HOME/Library/LaunchAgents/com.github.eve-bridge.daemon.plist"

arduino-cli compile --fqbn "$FQBN" --build-path "$BUILD" "$SKETCH"

# archive the exact ELF so a later panic backtrace stays symbolizable
mkdir -p past-experiments/firmware/build-archive
cp "$BUILD/claude_pet_eink_monitor.ino.elf" \
   "past-experiments/firmware/build-archive/claude_pet_eink_monitor-$(date +%Y%m%d-%H%M%S).elf"

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
