#pragma once
#include <stdint.h>
#include "persona.h"

// A pet that is showing words is awake.
//
// The persona sleeps whenever Claude Code is idle (derive() in main.cpp), and
// a voice conversation or a thought caption is not Claude Code activity. So
// eve used to close its eyes under its own reply, and the "zzz" status word
// flashed into the caption band whenever a page ran out before the next one
// arrived: the status row (y 186..204) sits on the caption band's 4th line
// (y 181). Owner, 2026-09-10: "the sleep appears in the middle of text".
//
// While a caption is up, and for kTextLingerMs after the last one, the face
// (eyes and status word) presents P_SLEEP as P_IDLE. Only the face: the body
// keeps its sleep pose, so eve does not chirp "sleepy" and drop its head
// between every page. Every other state passes through unchanged.
//
// Pure, no Arduino dependency: covered by host/face_test.cpp.

namespace face {

constexpr uint32_t kTextLingerMs = 4000;   // a gap between pages, and the reading beat after the last

// `textUp`: a caption is on screen this frame. `lastTextMs`: millis() of the
// last frame one was (0 = never). Unsigned subtraction keeps millis() rollover safe.
inline bool textRecent(bool textUp, uint32_t now, uint32_t lastTextMs) {
  return textUp || (lastTextMs != 0 && now - lastTextMs < kTextLingerMs);
}

inline PersonaState faceState(PersonaState active, bool textUp, uint32_t now, uint32_t lastTextMs) {
  return (active == P_SLEEP && textRecent(textUp, now, lastTextMs)) ? P_IDLE : active;
}

}  // namespace face
