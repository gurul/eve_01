#pragma once
#include <stdint.h>

// Age of a millis() stamp, safe when the stamp is newer than `now`.
//
// loop() reads `now = millis()` once at the top, then dataPoll() parses host
// commands and stamps them with a fresh millis(). When the clock ticks between
// the two, a stamp is 1 ms AFTER `now`, and the plain `now - at` wraps to about
// four billion. The agent-phase stale check (`now - agentAtMs > 30000`) then
// dropped a phase the instant it arrived: three times in one conversation on
// 2026-09-10 (board event ring "agent phase stale -> idle"), each followed by
// eve falling to the sleep persona under its own caption.
//
// elapsedMs() treats a stamp up to 2^31 ms in the future as age 0, and is still
// correct across the 49.7-day millis() rollover for real ages.
//
// Pure, no Arduino dependency: covered by host/ticks_test.cpp.

namespace ticks {

inline uint32_t elapsedMs(uint32_t now, uint32_t at) {
  return (int32_t)(now - at) < 0 ? 0u : now - at;
}

}  // namespace ticks
