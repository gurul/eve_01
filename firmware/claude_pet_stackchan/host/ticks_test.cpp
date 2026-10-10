// Host test for ticks.h. Build from the repo root:
//   clang++ -std=c++17 -I firmware/claude_pet_stackchan/src \
//     firmware/claude_pet_stackchan/host/ticks_test.cpp -o /tmp/ticks_test && /tmp/ticks_test
#include "ticks.h"

#include <cassert>
#include <cstdio>

using ticks::elapsedMs;

static void test_plain_age() {
  assert(elapsedMs(10000, 4000) == 6000);
  assert(elapsedMs(10000, 10000) == 0);
}

static void test_stamp_newer_than_now_is_age_zero() {
  // The bug: now read at the top of loop(), the command stamped 1 ms later.
  const uint32_t now = 1776071, at = now + 1;
  assert(now - at > 30000u);                 // the old check: "stale" on arrival
  assert(elapsedMs(now, at) == 0);           // the fix: brand new
  assert(!(elapsedMs(now, at) > 30000u));
}

static void test_rollover() {
  // Stamped 500 ms before millis() wraps, read 1500 ms after: 2000 ms old.
  assert(elapsedMs(1500u, 0xFFFFFFFFu - 499u) == 2000u);
}

static void test_stale_still_detected() {
  // Positive control: a phase 30.001 s old is still stale.
  assert(elapsedMs(100000, 100000 - 30001) > 30000u);
}

int main() {
  test_plain_age();
  test_stamp_newer_than_now_is_age_zero();
  test_rollover();
  test_stale_still_detected();
  std::puts("ticks_test: all passed");
  return 0;
}
