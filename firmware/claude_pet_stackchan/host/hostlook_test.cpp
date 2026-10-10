// Host test for hostlook.h. Build from the repo root:
//   clang++ -std=c++17 -I firmware/claude_pet_stackchan/src \
//     firmware/claude_pet_stackchan/host/hostlook_test.cpp -o /tmp/hostlook_test && /tmp/hostlook_test
#include "hostlook.h"

#include <cassert>
#include <cstdio>

using namespace hostlook;

static void test_conversation_takes_the_look_in_any_state() {
  // A voice request lands even when the persona is not calm and the owner is wanted.
  assert(accepted(false, false, true, true, false));
  assert(accepted(false, false, true, false, false));
}

static void test_card_and_dictation_always_refuse() {
  assert(!accepted(true, false, true, false, true));    // a permission card is up
  assert(!accepted(false, true, true, false, true));    // the owner is dictating
}

static void test_outside_a_conversation_the_old_rule_stands() {
  assert(accepted(false, false, false, false, true));   // calm: sleep / idle / busy
  assert(!accepted(false, false, false, false, false)); // not calm
  assert(!accepted(false, false, false, true, true));   // the owner is wanted
}

static void test_clamps_reach_behind_but_not_past_the_neck() {
  assert(clampYaw(120) == 120 && clampYaw(-120) == -120);
  assert(clampYaw(180) == 120 && clampYaw(-500) == -120);
  assert(clampYaw(-37) == -37);
  assert(clampPitch(0) == 5 && clampPitch(90) == 85 && clampPitch(45) == 45);
}

static void test_hold_window_and_rollover() {
  assert(holdsHead(true, 1000, 5000));                   // inside the hold
  assert(!holdsHead(true, 5000, 5000));                  // expired exactly at the end
  assert(!holdsHead(false, 1000, 5000));                 // not a host look
  // A hold stamped just before millis() wraps is still held just after the wrap.
  assert(holdsHead(true, 100u, 0xFFFFFF00u + 1000u));
}

int main() {
  test_conversation_takes_the_look_in_any_state();
  test_card_and_dictation_always_refuse();
  test_outside_a_conversation_the_old_rule_stands();
  test_clamps_reach_behind_but_not_past_the_neck();
  test_hold_window_and_rollover();
  std::printf("hostlook_test: all passed\n");
  return 0;
}
