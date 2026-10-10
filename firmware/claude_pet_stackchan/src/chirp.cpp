// R2D2 chirp synthesiser on M5.Speaker. Includes M5Unified (global ::M5);
// must never include board_compat.h — see hal_m5.h. Playback is asynchronous:
// a phrase is rendered into PSRAM in one pass (~1-2 ms) and the speaker task
// plays it back.
#include "chirp.h"
#include "ticks.h"
#include <M5Unified.hpp>
#include <esp_heap_caps.h>
#include <math.h>

// 16 kHz, not 8: the recipe's rising sweeps reach k + 10 Hz x 1000 steps,
// far above an 8 kHz Nyquist. 16 kHz keeps sweeps to 7 kHz clean and costs
// 32 KB per 2 s buffer, which lives in PSRAM.
static const uint32_t RATE        = 16000;
static const size_t   MAX_SAMPLES = RATE * 2;          // 2 s per phrase
static const float    F_MIN = 60.0f, F_MAX = 7000.0f;  // below Nyquist
static const size_t   RAMP = RATE * 2 / 1000;          // 2 ms attack/release

static uint8_t* bufs[2] = { nullptr, nullptr };
static uint8_t  wIdx    = 0;        // buffer being written next
static uint8_t* wbuf    = nullptr;
static size_t   len     = 0;
static uint32_t phase   = 0;        // phase accumulator, 2^32 = one cycle
static float    amp     = 0.65f;    // 0..1 of full scale
static bool     enabled = true;
static bool     ready   = false;
// isPlaying() clears when the mixer consumes the PCM, before the I2S DMA
// tail reaches the speaker. Allow 150 ms for the default 8 x 256 buffers
// at 48 kHz to drain before disabling the amplifier (no delay in loop()).
static constexpr uint32_t IDLE_DRAIN_MS = 150;
static uint32_t idleSince = 0;
static bool draining = false;

// ---- synthesis primitives ----
static inline uint8_t square(uint32_t ph, float env) {
  float v = (ph & 0x80000000u) ? 1.0f : -1.0f;
  return (uint8_t)(128 + (int)(v * 127.0f * amp * env));
}

// Linear sweep f0 -> f1 over ms. Phase continues across segments so joins
// never click; the 2 ms ramps soften the note edges.
static void seg(float f0, float f1, uint32_t ms) {
  size_t n = (size_t)ms * RATE / 1000;
  if (len + n > MAX_SAMPLES) n = MAX_SAMPLES - len;
  if (n == 0) return;
  for (size_t i = 0; i < n; i++) {
    float f = f0 + (f1 - f0) * (float)i / (float)n;
    if (f < F_MIN) f = F_MIN;
    if (f > F_MAX) f = F_MAX;
    uint32_t inc = (uint32_t)(f * (4294967296.0 / RATE));
    float env = 1.0f;
    if (i < RAMP)         env = (float)i / RAMP;
    if (n - i <= RAMP)    env = fminf(env, (float)(n - i) / RAMP);
    wbuf[len++] = square(phase, env);
    phase += inc;
  }
}
static void gap(uint32_t ms) {
  size_t n = (size_t)ms * RATE / 1000;
  if (len + n > MAX_SAMPLES) n = MAX_SAMPLES - len;
  for (size_t i = 0; i < n; i++) wbuf[len++] = 128;
}
static void note(float f, uint32_t ms) { seg(f, f, ms); }

// ---- the R2D2 recipe ----
// Original: tone() steps of 2 Hz (down/up) then 10 Hz (up/down) with
// a 0.9..2 ms pause per step — about 1.2 ms. Step counts are trimmed so a
// phrase fits the 2 s buffer with its beeps.
static const float STEP_MS = 1.2f;

static void phrase1(int k, int nDown, int nUp) {                  // down slow, up fast
  seg(k, k - 2.0f * nDown, (uint32_t)(nDown * STEP_MS));
  seg(k, k + 10.0f * nUp,  (uint32_t)(nUp * STEP_MS));
}
static void phrase2(int k, int nUp, int nDown) {                  // up slow, down fast
  seg(k, k + 2.0f * nUp,    (uint32_t)(nUp * STEP_MS));
  seg(k, k - 10.0f * nDown, (uint32_t)(nDown * STEP_MS));
}
static void beeps(int count) {                                    // K + (-1700..2000)
  const int K = 2000;
  for (int i = 0; i < count; i++) {
    note(K + random(-1700, 2000), random(70, 170));
    gap(random(0, 30));
  }
}

static void build(ChirpKind kind) {
  len = 0; phase = 0; amp = 0.65f;
  switch (kind) {
    case CHIRP_WAKE: {                          // ~0.35 s rising whistle
      int k = random(900, 1400);
      seg(k, k + 1500, 220);
      gap(20);
      note(k + 1800, 60);
      break;
    }
    case CHIRP_SLEEPY: {                        // phrase1, quiet, <= 1.1 s
      amp = 0.35f;
      phrase1(random(1000, 1600), random(300, 600), random(60, 120));
      break;
    }
    case CHIRP_ATTENTION: {                     // phrase2 + 4-6 excited beeps
      phrase2(random(1200, 2000), random(200, 500), random(60, 150));
      gap(40);
      beeps(random(4, 7));
      break;
    }
    case CHIRP_HAPPY: {                         // trill: 5-8 quick beeps rising
      int base = random(1200, 1800), n = random(5, 9);
      for (int i = 0; i < n; i++) { note(base + i * 250, random(60, 90)); gap(15); }
      break;
    }
    case CHIRP_LISTEN: {                        // "hm?" up-chirp
      int k = random(800, 1200);
      seg(k, k * 1.8f, 180);
      break;
    }
    case CHIRP_OK: {                            // beep-boop
      int f = random(1400, 1800);
      note(f, 90); gap(30); note(f * 0.7f, 110);
      break;
    }
    case CHIRP_NO: {                            // descending boop
      int k = random(900, 1200);
      seg(k, k * 0.5f, 250);
      break;
    }
    case CHIRP_CURIOUS: {                       // "oh?": two rising notes
      int k = random(900, 1300);
      note(k, 90); gap(40); seg(k * 1.3f, k * 1.9f, 160);
      break;
    }
    case CHIRP_SURPRISE: {                      // "!": one high blip
      note(random(2400, 3200), 110);
      break;
    }
    case CHIRP_SIGH: {                          // slow falling sweep, quiet
      amp = 0.4f;
      int k = random(1100, 1500);
      seg(k, k * 0.55f, 520);
      break;
    }
    case CHIRP_WARBLE: {                        // soft quick warble
      amp = 0.5f;
      int k = random(1300, 1700);
      for (int i = 0; i < 4; i++) seg(k + (i & 1 ? 150 : -150), k + (i & 1 ? -150 : 150), 55);
      break;
    }
    case CHIRP_STARTLE: {                       // sharp double blip
      int k = random(2600, 3400);
      note(k, 60); gap(25); note(k + 300, 80);
      break;
    }
    case CHIRP_TALK: {                          // babble: 4-6 short beeps, R2D2 cadence, ~0.6 s
      amp = 0.5f;
      int base = random(1100, 1900), n = random(4, 7);
      for (int i = 0; i < n; i++) {
        int f = base + random(-500, 700);
        if (random(3) == 0) seg(f, f + random(-300, 300), random(50, 90)); else note(f, random(45, 80));
        gap(random(15, 45));
      }
      break;
    }
    case CHIRP_CONFUSED: {                      // wobble around a base
      int k = random(1000, 1500);
      for (int i = 0; i < 6; i++) seg(k + (i & 1 ? 400 : -400), k + (i & 1 ? -400 : 400), 80);
      break;
    }
  }
}

// ---- playback ----
static bool start(bool force) {
  if (len == 0) return false;
  draining = false;
  // Check begin explicitly: some M5Unified versions return true from
  // playRaw even when the lazy speaker startup fails.
  if (!M5.Speaker.begin()) {
    M5.Speaker.end();
    return false;
  }
  bool ok = M5.Speaker.playRaw(wbuf, len, RATE, false, 1, -1, force);
  if (ok) wIdx ^= 1;                            // next write goes to the other buffer
  return ok;
}

static bool canStart(bool force) {
  if (!ready || !enabled) return false;
  if (M5.Speaker.isPlaying() && !force) return false;
  wbuf = bufs[wIdx];
  return wbuf != nullptr;
}

void chirpBegin() {
  for (int i = 0; i < 2; i++) {
    bufs[i] = (uint8_t*)heap_caps_malloc(MAX_SAMPLES, MALLOC_CAP_SPIRAM | MALLOC_CAP_8BIT);
    if (!bufs[i]) bufs[i] = (uint8_t*)malloc(MAX_SAMPLES);   // no PSRAM: internal
  }
  ready = bufs[0] && bufs[1];
  Serial.printf("[chirp] buffers %s (%u x 2 bytes, psram=%d)\n",
                ready ? "ready" : "FAILED", (unsigned)MAX_SAMPLES, (int)psramFound());
}

void chirpSetEnabled(bool on) {
  enabled = on;
  if (!on && M5.Speaker.isRunning()) {
    M5.Speaker.end();   // cancel current sound AND disable the amplifier
    draining = false;
  }
}
bool chirpPlaying() { return M5.Speaker.isPlaying(); }
void chirpUpdate() {
  if (!M5.Speaker.isRunning() || M5.Speaker.isPlaying()) {
    draining = false;
    return;
  }
  const uint32_t now = millis();
  if (!draining) {
    idleSince = now;
    draining = true;
  }
  if (ticks::elapsedMs(now, idleSince) >= IDLE_DRAIN_MS) {
    // stop() or volume zero leaves the CoreS3 amp enabled and can hiss.
    // end() invokes M5Unified's board-specific amplifier disable callback.
    M5.Speaker.end();
    draining = false;
    Serial.println("[chirp] speaker off (idle)");
  }
}

void chirpPlay(ChirpKind kind, bool force) {
  if (!canStart(force)) return;
  build(kind);
  start(force);
}

void chirpBeep(uint16_t freq, uint16_t ms) {
  if (!canStart(false)) return;
  len = 0; phase = 0; amp = 0.6f;
  note(freq, ms);
  start(false);
}
