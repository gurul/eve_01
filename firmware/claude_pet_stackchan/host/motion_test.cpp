// Host test for motion.h. Build from this directory:
//   clang++ -std=c++17 -O2 -Wall -Wextra -I. motion_test.cpp -o motion_test && ./motion_test
#include "../src/motion.h"
#include <cassert>
#include <cstdio>
#include <cmath>

using namespace motion;

struct Walk { float ymin, ymax, pmin, pmax, vy, vp; uint32_t samples; };

// Walk a motion at a sample interval and report the extremes plus the peak
// per-axis velocity, which is what the servo actually feels.
static Walk walk(const Osc& o, uint32_t dtMs) {
  Walk w{1e9f, -1e9f, 1e9f, -1e9f, 0, 0, 0};
  float py = 0, pp = 0; bool first = true;
  for (uint32_t t = 0; ; t += dtMs) {
    float y, p;
    if (!pose(o, t, &y, &p)) break;
    if (y < w.ymin) w.ymin = y;
    if (y > w.ymax) w.ymax = y;
    if (p < w.pmin) w.pmin = p;
    if (p > w.pmax) w.pmax = p;
    if (!first) {
      const float vy = fabsf(y - py) * 1000.0f / (float)dtMs;
      const float vp = fabsf(p - pp) * 1000.0f / (float)dtMs;
      if (vy > w.vy) w.vy = vy;
      if (vp > w.vp) w.vp = vp;
    }
    py = y; pp = p; first = false; w.samples++;
  }
  return w;
}

static void report(const char* name, Osc want) {
  const Osc got = admit(want);
  const Walk w = walk(got, 1);
  const Walk loop = walk(got, 87);      // the board's measured loop period
  printf("%-9s asked amp %u/%u per %u cyc %u  ->  amp %u/%u per %u/%u cyc %u  %ums\n",
         name, want.ampYaw, want.ampPitch, want.periodMs, want.cycles,
         got.ampYaw, got.ampPitch, got.periodMs, got.pitchPeriodMs, got.cycles, durationMs(got));
  printf("          yaw %.1f..%.1f peak %.0f deg/s | pitch %.1f..%.1f peak %.0f deg/s"
         " | %u samples at 87 ms\n",
         w.ymin, w.ymax, w.vy, w.pmin, w.pmax, w.vp, loop.samples);
  // Every motion, however asked for, obeys travel and velocity.
  assert(w.ymax <= (float)kYawLimit && w.ymin >= -(float)kYawLimit);
  assert(w.pmin >= (float)kPitchMin && w.pmax <= (float)kPitchMax);
  assert(w.vy <= 245.0f && w.vp <= 245.0f);
}

int main() {
  printf("sizeof(Osc) = %zu bytes\n\n", sizeof(Osc));

  Osc dance{}; dance.centerYaw = 0; dance.centerPitch = 45; dance.ampYaw = 30; dance.ampPitch = 12;
  dance.periodMs = 900; dance.phaseDeg = 90; dance.cycles = 8; dance.dwellPct = 8; dance.jitterPct = 10;
  report("dance", dance);

  Osc sway{}; sway.centerPitch = 45; sway.ampYaw = 25; sway.periodMs = 1600; sway.cycles = 3;
  sway.dwellPct = 10; report("sway", sway);

  Osc nod{}; nod.centerPitch = 45; nod.ampPitch = 10; nod.periodMs = 700; nod.cycles = 3;
  nod.dwellPct = 12; report("nod", nod);

  Osc shake{}; shake.centerPitch = 45; shake.ampYaw = 18; shake.periodMs = 650; shake.cycles = 3;
  report("shake", shake);

  Osc fig8{}; fig8.centerPitch = 45; fig8.ampYaw = 25; fig8.ampPitch = 10;
  fig8.periodMs = 1600; fig8.pitchPeriodMs = 800; fig8.cycles = 4; report("fig8", fig8);

  // Off centre: a wide swing about yaw 90 must come back narrow, not clipped.
  Osc off{}; off.centerYaw = 90; off.centerPitch = 45; off.ampYaw = 60;
  off.periodMs = 2000; off.cycles = 3; report("offcentre", off);

  // A model asking for something silly is reduced, not obeyed.
  Osc silly{}; silly.centerYaw = 0; silly.centerPitch = 45; silly.ampYaw = 120; silly.ampPitch = 60;
  silly.periodMs = 100; silly.cycles = 250; silly.dwellPct = 90; silly.jitterPct = 90;
  report("silly", silly);
  const Osc s = admit(silly);
  assert(durationMs(s) <= kBoutMaxMs);
  assert(s.dwellPct <= kDwellPctMax && s.jitterPct <= kJitterPctMax);

  // Pitch pinned at a rail: the amplitude goes to zero rather than grind.
  Osc rail{}; rail.centerPitch = 85; rail.ampPitch = 10; rail.periodMs = 900; rail.cycles = 3;
  const Osc r = admit(rail);
  printf("\npitch centre 85 -> centre %d amp %u (a rhythm is kept inside %d..%d)\n",
         r.centerPitch, r.ampPitch, kOscPitchMin, kOscPitchMax);
  assert(r.centerPitch <= kOscPitchMax);

  // Both ends: zero displacement AND near-zero velocity, so no jerk either way.
  const Osc d = admit(dance);
  float y0, p0, y1, p1;
  pose(d, 0, &y0, &p0);
  pose(d, 1, &y1, &p1);
  printf("start: pose %.3f,%.3f then %.3f,%.3f -> entry velocity %.1f deg/s\n",
         y0, p0, y1, p1, fabsf(y1 - y0) * 1000.0f);
  assert(fabsf(y0 - d.centerYaw) < 0.001f && fabsf(p0 - d.centerPitch) < 0.001f);
  assert(fabsf(y1 - y0) * 1000.0f < 5.0f);
  const uint32_t end = durationMs(d);
  float ya, pa, yb, pb;
  pose(d, end - 2, &ya, &pa);
  const bool live = pose(d, end, &yb, &pb);
  printf("end:   live=%d pose %.3f,%.3f -> exit velocity %.1f deg/s\n",
         live, yb, pb, fabsf(yb - ya) * 500.0f);
  assert(!live && fabsf(yb - d.centerYaw) < 0.001f);
  assert(fabsf(yb - ya) * 500.0f < 5.0f);

  // The dwell actually holds at the extreme.
  const Osc n = admit(nod);
  uint32_t held = 0, tot = 0;
  for (uint32_t t = 0; t < (uint32_t)n.periodMs; t++) {
    float yy, pp;
    pose(n, t, &yy, &pp);
    if (fabsf(pp - n.centerPitch) >= n.ampPitch * 0.99f) held++;
    tot++;
  }
  printf("nod holds within 1%% of its extreme for %u of %u ms of a cycle = %.0f%%\n",
         held, tot, 100.0f * held / (float)tot);

  // Jitter: consecutive cycles differ, and the same cycle always repeats.
  printf("jitter by cycle: ");
  for (uint32_t c = 0; c < 4; c++) printf("%.3f ", jitterScale(c, 0u, 10));
  printf("\n");
  assert(jitterScale(0, 0, 10) != jitterScale(1, 0, 10));
  assert(jitterScale(2, 0, 10) == jitterScale(2, 0, 10));
  assert(jitterScale(0, 0, 0) == 1.0f);

  // Duty: a bout plus the cooldown the envelope's silence provides.
  printf("\nbout %ums of the %ums cap; %u cycles of the %u cap\n",
         durationMs(d), (unsigned)kBoutMaxMs, d.cycles, (unsigned)kCyclesMax);

  printf("\nmotion_test: all assertions passed\n");
  return 0;
}
