#pragma once
#include <stdint.h>
#include <string.h>
#include <math.h>
#include "ticks.h"

// Transient semantic eyes only. No pose or actuator fields.
namespace expression {
enum Kind : uint8_t { None, Calm, Happy, Curious, Affection, Surprised, Startled, Sad, Worried, Skeptical, Frustrated, Excited, Wink };
inline const char* name(Kind k) {
  static const char* names[] = {"none", "calm", "happy", "curious", "affection", "surprised", "startled", "sad", "worried", "skeptical", "frustrated", "excited", "wink"};
  return k <= Wink ? names[k] : "none";
}
inline Kind parse(const char* s) {
  if (s) for (unsigned i = Calm; i <= Wink; ++i)
    if (!strcmp(s, name((Kind)i))) return (Kind)i;
  return None;
}
// Distinct eye shapes; mood: 0 neutral, 1 smile, 2 droop, 3 inward brow.
struct Style { uint8_t left, right, width, radius, mood, blink; bool curious; };
inline Style style(Kind kind) {
  switch (kind) {
    case Wink:       return {96, 96, 96, 22, 0, 4, false};
    case Happy:      return {86, 86, 96, 22, 1, 3, false};
    case Curious:    return {100, 88, 96, 22, 0, 3, true};
    case Affection:  return {76, 76, 100, 32, 1, 5, true};
    case Surprised:  return {110, 110, 84, 36, 0, 5, false};
    case Sad:        return {72, 72, 92, 22, 2, 5, false};
    case Worried:    return {102, 94, 90, 26, 2, 2, true};
    case Skeptical:  return {58, 96, 94, 18, 0, 4, true};
    case Frustrated: return {70, 70, 94, 16, 3, 2, false};
    case Excited:    return {110, 110, 104, 26, 1, 1, false};
    default:        return {96, 96, 96, 22, 0, 3, false};
  }
}
// Same rounded-square lid: flat middle, 22 px corner zones, shallow 8 px rise.
constexpr uint32_t winkHoldMs = 650;
inline int winkCurveY(int x, int width) {
  if (width <= 0) return 0;
  int radius = width / 2 < 22 ? width / 2 : 22;
  if (radius <= 0) return 0;
  int edge = x < width - x ? x : width - x;
  if (edge >= radius) return -8;
  float t = (float)(radius - edge) / radius;
  return -(int)lroundf(8.0f * sqrtf(1.0f - t * t));
}

struct WinkHold {
  uint32_t at = 0;
  bool running = false;
  void start(uint32_t now) { at = now; running = true; }
  bool active(uint32_t now) const { return running && ticks::elapsedMs(now, at) < winkHoldMs; }
};
struct Request { uint32_t id = 0; Kind kind = None; uint32_t ttl = 4000; };
struct State {
  Request request;
  uint32_t at = 0;
  bool active(uint32_t now) const {
    return request.kind != None && ticks::elapsedMs(now, at) < request.ttl;
  }
  bool accept(Request next, uint32_t now) {
    if (!next.id || next.kind > Wink || (request.id && (int32_t)(next.id - request.id) <= 0)) return false;
    next.ttl = next.ttl < 500 ? 500 : next.ttl > 6000 ? 6000 : next.ttl;
    request = next; at = now;
    return true;
  }

};
inline bool allowed(bool hidden, bool attention, bool listening, bool asking, bool error) {
  return !hidden && !attention && !listening && !asking && !error;
}
}
