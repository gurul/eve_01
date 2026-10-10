#pragma once
// motion.h — the board's motion vocabulary: one parametric two-axis
// oscillator, sanitized before it runs.
//
// Pure C++. No Arduino, no clock, no randomness, no allocation — the caller
// passes elapsed time, exactly like mood.h. Host-testable like hostlook.h,
// ticks.h and face.h, so the hardware envelope below is checked by a test
// rather than by a bench run on a live robot.
//
// Why an oscillator and not more keyframes: headTo() grants every keyframe a
// 300 ms glide floor (150 ms at speed >= 800) and re-eases from rest at each
// key, so a fast beat gets three or four samples of an ease-in-out and reads
// as stepped. A continuous phase function has no floor and no discretisation:
// it is sampled at whatever rate the loop runs, and the BSP's 50 Hz spring
// interpolates between samples. stepTween()'s micro-drift is already exactly
// such a sampled sine, which is the proof the pattern works on this hardware.
#include <stdint.h>
#include <math.h>

namespace motion {

// ---------------------------------------------------------------------------
// Hardware envelope. Every limit is enforced by SHRINKING THE REQUEST before
// the motion runs, never by clipping a sample: clipping samples would
// flat-top the swing against the travel limit and hold the servo at its own
// end stop for part of every cycle, which is the failure this design exists
// to avoid.
// ---------------------------------------------------------------------------

// Travel. The same numbers body.cpp and hostlook.h enforce.
constexpr int kYawLimit = 120, kPitchMin = 5, kPitchMax = 85;

// A rhythm stays clear of the travel limits, so a peak can never reach one.
constexpr int kOscPitchMin = 20, kOscPitchMax = 70;

// Per-axis amplitude ceilings. Yaw is gravity-neutral with 240 deg of travel;
// pitch carries the head against gravity with 80 deg, which is why M5Stack's
// own firmware enables stall protection on pitch and not on yaw.
constexpr int kAmpYawMax = 45, kAmpPitchMax = 15;

// Velocity budget, in degrees x hertz. The SCS0009 life test (datasheet 8-1:
// forward 60 deg in 0.25 s, stop 0.5 s, reverse 60 deg in 0.25 s, stop 0.5 s,
// one cycle; load 1/5 stall torque, > 50,000 cycles at 6 V) qualifies
// 60 deg / 0.25 s = 240 deg/s. For a sinusoid peak velocity is A * 2 * pi * f,
// so A(deg) * f(Hz) <= 240 / (2*pi) = 38.2 runs the axis no faster than the
// manufacturer qualified. Pitch gets half of it: it is the loaded axis.
constexpr float kAmpFreqYaw = 34.0f, kAmpFreqPitch = 17.0f;
constexpr float kVelMaxDegPerSec = 240.0f;   // the life test's own mean velocity

// Period floor. At the spring stiffness stepTween's speed cap of 600 selects,
// k = 10 + 0.6^2 * 640 = 240 and the natural frequency is sqrt(240) =
// 15.5 rad/s = 2.47 Hz, so above roughly 1.5 Hz the follower eats the
// amplitude and the request buys heat instead of motion.
constexpr uint16_t kPeriodMin = 650, kPeriodMax = 8000;

// One bout. M5Stack's longest shipped dance is 7.5 s. A motion that never
// ends holds torque for ever, which defeats the only servo protection this
// robot has (body.cpp:137 stops streaming, servo.cpp:59-66 then releases).
constexpr uint16_t kBoutMaxMs = 8000;
constexpr uint8_t  kCyclesMax = 12;

// Aliveness shaping, both capped because both raise peak velocity.
// dwellPct flat-tops the sine so the head pauses at each extreme the way a
// creature does and a sine does not. jitterPct varies amplitude per cycle so
// the motion is not perfectly periodic, which is the tell of a machine.
constexpr uint8_t kDwellPctMax = 20, kJitterPctMax = 20;

// Amplitude ramp length, as a fraction of the period. Measured on the host
// (scratchpad probe2): a raised-cosine ramp of 0.25 period lets the rising
// envelope and the rising sine compound and raises peak velocity 15.5% above
// A*2*pi*f, which would silently break the velocity budget. At 0.35 the
// overshoot is gone (ratio 1.001) and the first swing still reaches 90% of
// amplitude. Longer ramps cost the first beat: 0.5 gives only 65%.
constexpr float kRampFrac = 0.35f;

// The keyframe form, for a motion whose SHAPE is the point rather than its
// rhythm — a double take, a lean-and-peek, a shrug. This is byte for byte the
// struct body.cpp's player already steps; moving it here is what lets the host
// send one. KEEP leaves that axis at its last commanded target.
constexpr int8_t  KEEP     = 127;
constexpr uint8_t kKeysMax = 8;            // body.cpp's dyn[] budget
constexpr uint16_t kKeyGapMinMs = 120;     // no key may land inside another's glide floor
struct Key { uint16_t atMs; int8_t yaw; int8_t pitch; uint16_t speed; };

struct Osc {
  int16_t  centerYaw, centerPitch;   // the pose the rhythm is about
  uint8_t  ampYaw, ampPitch;         // degrees either side of centre
  uint16_t periodMs;                 // yaw period
  uint16_t pitchPeriodMs;            // 0 = the same period as yaw
  int16_t  phaseDeg;                 // pitch lead: 0 a diagonal, 90 a circle
  uint8_t  cycles;
  uint8_t  dwellPct;
  uint8_t  jitterPct;
};

inline int clampi(int v, int lo, int hi) { return v < lo ? lo : v > hi ? hi : v; }

// Shrink a request until it is inside the envelope, and return the safe copy.
// AMPLITUDE yields, not the period: when the owner asks for a faster dance the
// rhythm is the salient thing, so a request that is too fast for its swing
// comes back with a narrower swing at the beat that was asked for.
inline Osc admit(Osc o) {
  o.centerYaw   = (int16_t)clampi(o.centerYaw, -kYawLimit, kYawLimit);
  o.centerPitch = (int16_t)clampi(o.centerPitch, kOscPitchMin, kOscPitchMax);
  o.periodMs      = (uint16_t)clampi(o.periodMs ? o.periodMs : 900, kPeriodMin, kPeriodMax);
  o.pitchPeriodMs = o.pitchPeriodMs
                  ? (uint16_t)clampi(o.pitchPeriodMs, kPeriodMin, kPeriodMax) : o.periodMs;
  o.phaseDeg  = (int16_t)(((o.phaseDeg % 360) + 360) % 360);
  o.dwellPct  = (uint8_t)clampi(o.dwellPct, 0, kDwellPctMax);
  o.jitterPct = (uint8_t)clampi(o.jitterPct, 0, kJitterPctMax);

  // The shaping gain is the factor by which dwell and jitter can push a peak
  // past the nominal amplitude. Every amplitude limit is divided by it, so the
  // limits hold for the shaped, jittered motion and not just the plain sine.
  const float gain = (1.0f + o.dwellPct / 100.0f) * (1.0f + o.jitterPct / 100.0f);

  int ay = clampi(o.ampYaw, 0, kAmpYawMax);
  int ap = clampi(o.ampPitch, 0, kAmpPitchMax);

  // Travel headroom about this centre.
  const int absCy = o.centerYaw < 0 ? -o.centerYaw : o.centerYaw;
  ay = clampi(ay, 0, (int)((kYawLimit - absCy) / gain));
  const int up = kOscPitchMax - o.centerPitch, down = o.centerPitch - kOscPitchMin;
  ap = clampi(ap, 0, (int)((up < down ? up : down) / gain));

  // Velocity budget, each axis at its own frequency.
  const float fy = 1000.0f / (float)o.periodMs;
  const float fp = 1000.0f / (float)o.pitchPeriodMs;
  ay = clampi(ay, 0, (int)(kAmpFreqYaw   / (fy * gain)));
  ap = clampi(ap, 0, (int)(kAmpFreqPitch / (fp * gain)));

  o.ampYaw = (uint8_t)ay; o.ampPitch = (uint8_t)ap;

  // Bout length: shed cycles rather than shorten the beat.
  uint8_t cyc = (uint8_t)clampi(o.cycles ? o.cycles : 4, 1, kCyclesMax);
  while (cyc > 1 && (uint32_t)cyc * o.periodMs > kBoutMaxMs) cyc--;
  o.cycles = cyc;
  return o;
}

// Total run time, the ramp-out included.
inline uint32_t durationMs(const Osc& o) { return (uint32_t)o.cycles * o.periodMs; }

// Raised-cosine amplitude ramp over the first and last quarter period. Phase 0
// has zero DISPLACEMENT but maximum VELOCITY, so without this a motion would
// start and end with a jerk. With it both ends have zero displacement and zero
// velocity: the motion grows out of the pose the head is already holding and
// dies back into it, and then the stream falls under stepTween's 0.05 deg gate
// so the BSP releases torque.
inline float envelope(const Osc& o, uint32_t t) {
  const uint32_t total = durationMs(o);
  if (t >= total) return 0.0f;
  const float ramp = (float)o.periodMs * kRampFrac;
  const float a = (float)t < ramp ? (float)t / ramp : 1.0f;
  const float b = (float)(total - t) < ramp ? (float)(total - t) / ramp : 1.0f;
  const float e = a < b ? a : b;
  return 0.5f * (1.0f - cosf(3.14159265f * e));
}

// Deterministic per-cycle amplitude scale in [1-j, 1+j]. A hash of the cycle
// index, not a PRNG: the same request always produces the same motion, so a
// host test can check it.
inline float jitterScale(uint32_t cycle, uint32_t salt, uint8_t jitterPct) {
  if (!jitterPct) return 1.0f;
  uint32_t h = (cycle + salt) * 2654435761u;
  h ^= h >> 15;
  const float u = (float)((h >> 8) & 0xFF) / 255.0f;
  return 1.0f + (2.0f * u - 1.0f) * (jitterPct / 100.0f);
}

// Flat-topped sine: gain then clip to +-1 holds the extreme for part of each
// half cycle. The clip is on the SHAPE, in units of amplitude — never on the
// pose in degrees, so it can never hold the servo against a travel limit.
inline float shaped(float theta, uint8_t dwellPct) {
  float s = sinf(theta);
  if (!dwellPct) return s;
  s *= (1.0f + dwellPct / 100.0f);
  return s < -1.0f ? -1.0f : s > 1.0f ? 1.0f : s;
}

// Sample the motion. `t` is milliseconds since it started. Writes the absolute
// pose and returns false once the motion is over, at which point the caller
// stops streaming and hands the head back.
inline bool pose(const Osc& o, uint32_t t, float* yaw, float* pitch) {
  if (t >= durationMs(o)) { *yaw = (float)o.centerYaw; *pitch = (float)o.centerPitch; return false; }
  const float env = envelope(o, t);
  const float phy = (float)t / (float)o.periodMs;
  const float php = (float)t / (float)o.pitchPeriodMs + (float)o.phaseDeg / 360.0f;
  // Each axis steps its jitter at its OWN zero crossing, where the shape is 0
  // and a change of amplitude cannot move the pose. Keying pitch off the yaw
  // cycle put its step at the pitch EXTREME whenever phaseDeg was 90, which
  // measured as a 2.4 deg jump, i.e. 1751 deg/s (host test, before this fix).
  *yaw   = (float)o.centerYaw   + env * (float)o.ampYaw
         * jitterScale((uint32_t)phy, 0u,  o.jitterPct) * shaped(6.2831853f * phy, o.dwellPct);
  *pitch = (float)o.centerPitch + env * (float)o.ampPitch
         * jitterScale((uint32_t)php, 77u, o.jitterPct) * shaped(6.2831853f * php, o.dwellPct);
  return true;
}

// Make a host-supplied keyframe list safe in place, and return how many keys
// survive: angles clamped to the travel limits, times forced to rise with at
// least kKeyGapMinMs between keys, and the list truncated at kBoutMaxMs.
inline uint8_t admitKeys(Key* k, uint8_t n) {
  if (n > kKeysMax) n = kKeysMax;
  uint16_t last = 0;
  for (uint8_t i = 0; i < n; i++) {
    if (k[i].yaw != KEEP)   k[i].yaw   = (int8_t)clampi(k[i].yaw, -kYawLimit, kYawLimit);
    if (k[i].pitch != KEEP) k[i].pitch = (int8_t)clampi(k[i].pitch, kPitchMin, kPitchMax);
    k[i].speed = (uint16_t)clampi(k[i].speed ? k[i].speed : 500, 100, 1000);
    if (i == 0) {
      k[i].atMs = 0;
    } else {
      // Spacing must satisfy the same velocity budget the oscillator obeys:
      // a key 240 deg from its predecessor cannot land 5 ms later. Time
      // stretches; the shape, which is the whole point of a keyframe list,
      // is kept. Without this an 8-key list demanded 2000 deg/s (host test).
      const int dy = (k[i].yaw   == KEEP || k[i-1].yaw   == KEEP) ? 0 : k[i].yaw   - k[i-1].yaw;
      const int dp = (k[i].pitch == KEEP || k[i-1].pitch == KEEP) ? 0 : k[i].pitch - k[i-1].pitch;
      const int ady = dy < 0 ? -dy : dy, adp = dp < 0 ? -dp : dp;
      const int dmax = ady > adp ? ady : adp;
      uint16_t need = (uint16_t)(1000.0f * (float)dmax / kVelMaxDegPerSec);
      if (need < kKeyGapMinMs) need = kKeyGapMinMs;
      if (k[i].atMs < last + need) k[i].atMs = (uint16_t)(last + need);
    }
    if (k[i].atMs > kBoutMaxMs) return i;
    last = k[i].atMs;
  }
  return n;
}

}  // namespace motion
