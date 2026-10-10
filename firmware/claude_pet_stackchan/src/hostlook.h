#pragma once
// Host-look rules ({"cmd":"look","yaw","pitch","hold"}), kept pure so
// host/hostlook_test.cpp can check them without a board.
//
// The owner can now tell eve where to look by voice ("look behind you",
// "a bit up and to your left"). That request outranks the robot's own
// head choreography for as long as its hold lasts:
//   - it is taken during a voice conversation whatever the persona state,
//     because the owner asked for it out loud;
//   - outside a conversation the old rule stands: calm states only
//     (sleep / idle / busy), never while the owner is wanted;
//   - never with a permission card up or while the dictation key is held.
// While it holds, the conversation's phase poses and micro-beats stand
// aside (body.cpp), so eve keeps looking where it was told while it
// answers, instead of snapping back to centre on the next phase change.
#include <stdint.h>

namespace hostlook {

constexpr int kYawLimitDeg = 120;              // body.cpp YAW_MAX: as far as the neck turns
constexpr int kPitchMinDeg = 5, kPitchMaxDeg = 85;

inline bool accepted(bool cardUp, bool listening, bool conversation, bool ownerWanted, bool calmState) {
  if (cardUp || listening) return false;
  if (conversation) return true;
  return !ownerWanted && calmState;
}

inline int clampYaw(int y) { return y < -kYawLimitDeg ? -kYawLimitDeg : y > kYawLimitDeg ? kYawLimitDeg : y; }
inline int clampPitch(int p) { return p < kPitchMinDeg ? kPitchMinDeg : p > kPitchMaxDeg ? kPitchMaxDeg : p; }

// A host look owns the head from the moment it lands until its hold runs out.
// Rollover-safe: the same signed-difference test body.cpp's gazeHeld() uses.
inline bool holdsHead(bool fromHost, uint32_t now, uint32_t until) {
  return fromHost && (int32_t)(now - until) < 0;
}

}  // namespace hostlook
