// Host test for face.h. Build from the repo root:
//   clang++ -std=c++17 -I firmware/claude_pet_stackchan/src \
//     firmware/claude_pet_stackchan/host/face_test.cpp -o /tmp/face_test && /tmp/face_test
#include "face.h"

#include <cassert>
#include <cstdio>

using face::faceState;
using face::kTextLingerMs;

static void test_sleep_with_text_up_is_awake() {
  assert(faceState(P_SLEEP, true, 10000, 10000) == P_IDLE);
}

static void test_sleep_lingers_awake_between_pages() {
  // A page ran out at t=10000; the next arrives inside the linger: no zzz in the gap.
  assert(faceState(P_SLEEP, false, 10000 + kTextLingerMs - 1, 10000) == P_IDLE);
}

static void test_sleep_returns_after_the_linger() {
  // Positive control for the rule: without recent text the pet still sleeps.
  assert(faceState(P_SLEEP, false, 10000 + kTextLingerMs, 10000) == P_SLEEP);
  assert(faceState(P_SLEEP, false, 50000, 0) == P_SLEEP);          // never showed text
}

static void test_other_states_pass_through() {
  const PersonaState all[] = {P_IDLE, P_BUSY, P_ATTENTION, P_CELEBRATE, P_DIZZY, P_HEART};
  for (PersonaState s : all) {
    assert(faceState(s, true, 10000, 10000) == s);
    assert(faceState(s, false, 10000, 0) == s);
  }
}

static void test_millis_rollover() {
  // Text last seen 1 s before millis() wraps; 2 s later it has wrapped to ~1000.
  const uint32_t last = 0xFFFFFFFFu - 999u;
  assert(faceState(P_SLEEP, false, 1000u, last) == P_IDLE);        // 2000 ms elapsed: still lingering
  assert(faceState(P_SLEEP, false, 5000u, last) == P_SLEEP);       // 6000 ms elapsed: asleep again
}

int main() {
  test_sleep_with_text_up_is_awake();
  test_sleep_lingers_awake_between_pages();
  test_sleep_returns_after_the_linger();
  test_other_states_pass_through();
  test_millis_rollover();
  std::puts("face_test: all passed");
  return 0;
}
