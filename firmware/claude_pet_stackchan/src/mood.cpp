// Affect engine — see mood.h. Pure C++ (compiled on the host too).
#include "mood.h"
#include <math.h>

float moodClamp(float x, float lo, float hi) { return x < lo ? lo : x > hi ? hi : x; }

static inline float decayToward(float x, float target, float dtS, float tauS) {
  // x <- target + (x - target) * e^(-dt/tau)
  return target + (x - target) * expf(-dtS / tauS);
}

void MoodEngine::reset() { *this = MoodEngine(); }

MoodKind MoodEngine::step(const MoodInput& in, uint32_t dtMs) {
  if (dtMs > 5000) dtMs = 5000;                 // a hiccup must not integrate a huge step
  const float dt = dtMs / 1000.0f;
  sinceFaceMs += dtMs; sinceMotionMs += dtMs; sinceTouchMs += dtMs; sinceOwnerMs += dtMs;
  sinceSurpriseMs += dtMs; sinceStartleMs += dtMs; sinceChirpMs += dtMs;

  // ---- stimuli (fixed deltas; MiRo/Mini style) ----
  if (in.motionConf > 0) {
    float m = in.motionConf / 100.0f;
    // A sudden strong motion while calm is a startle; ordinary motion is interest.
    bool sudden = in.motionConf >= 60 && sinceMotionMs > 3000 && a < 0.3f;
    if (sudden && !in.asleep && sinceStartleMs > STARTLE_COOLDOWN_MS) {
      a += 0.7f; v -= 0.35f;
      sinceStartleMs = 0;
    } else {
      a += 0.2f * m * dt * 4.0f;                // ~+0.2 per second of solid motion
    }
    stimulation -= 0.3f * m * dt;
    sinceMotionMs = 0;
  }
  if (in.faceSeen) {
    sinceFaceMs = 0;
    social -= 0.4f * dt * 2.0f;                 // company satisfies the social drive fast
    if (in.faceOwner) {
      if (sinceOwnerMs > 20000) { v += 0.4f; a += 0.2f; }   // "you're back!" once per visit
      sinceOwnerMs = 0;
    } else if (sinceFaceMs == 0 && sinceMotionMs > 500) {
      a += 0.15f * dt * 4.0f; v += 0.05f * dt * 4.0f;       // a stranger: interested, a little wary
    }
  }
  if (in.touched) {
    v += 0.2f; a += 0.1f; social -= 0.2f;
    sinceTouchMs = 0;
  }
  if (in.newView) {
    a += 0.1f; stimulation -= 0.15f;
    if (a > 0.25f) sinceSurpriseMs = 0;         // arriving somewhere new while keen = a small surprise
  }
  if (in.hostEmote) {
    v += moodClamp(in.dv / 100.0f, -HOST_EMOTE_CLAMP, HOST_EMOTE_CLAMP);
    a += moodClamp(in.da / 100.0f, -HOST_EMOTE_CLAMP, HOST_EMOTE_CLAMP);
  }

  // ---- drives drift when unsatisfied ----
  if (sinceFaceMs > 30000 && sinceTouchMs > 30000) social += SOCIAL_RISE_PER_S * dt;
  if (sinceMotionMs > 15000 && !in.newView) stimulation += STIM_RISE_PER_S * dt;
  social = moodClamp(social, 0.0f, 1.0f);
  stimulation = moodClamp(stimulation, 0.0f, 1.0f);
  // Unsatisfied drives colour the emotion (Kismet regimes): bored = flat,
  // lonely = down. A pull toward a target (not an unbounded push) so the
  // feeling settles at "bored", never at the rail; the pull (tau 20 s) is
  // stronger than the relaxation toward the mood (tau 60 s) on purpose.
  if (stimulation > 0.6f) {
    float k = moodClamp((stimulation - 0.6f) / 0.1f, 0.0f, 1.0f);
    a = decayToward(a, -0.6f * k, dt, 20.0f);
  }
  if (social > 0.7f) {
    float k = moodClamp((social - 0.7f) / 0.1f, 0.0f, 1.0f);
    v = decayToward(v, -0.7f * k, dt, 20.0f);
  }
  if (in.asleep) { a = decayToward(a, -0.6f, dt, 20.0f); }

  // ---- relax toward the mood, mood toward the emotion ----
  v = decayToward(v, moodV, dt, TAU_EMOTION_S);
  a = decayToward(a, moodA, dt, TAU_EMOTION_S);
  moodV = decayToward(moodV, v, dt, TAU_MOOD_S);
  moodA = decayToward(moodA, a, dt, TAU_MOOD_S);
  v = moodClamp(v, -1.0f, 1.0f); a = moodClamp(a, -1.0f, 1.0f);
  moodV = moodClamp(moodV, -0.5f, 0.5f); moodA = moodClamp(moodA, -0.5f, 0.5f);

  classify();
  express();
  return kind;
}

void MoodEngine::classify() {
  MoodKind k;
  if (sinceStartleMs < 1500)                                  k = MOOD_STARTLED;
  else if (a > 0.6f && v < -0.2f)                             k = MOOD_STARTLED;
  else if (sinceSurpriseMs < 2500 && a > 0.3f)                k = MOOD_SURPRISED;
  else if (sinceOwnerMs < 20000 && v > 0.2f)                  k = MOOD_AFFECTION;
  else if (v > 0.4f && a > -0.2f)                             k = MOOD_HAPPY;
  else if (a > 0.2f && v >= -0.2f)                            k = MOOD_CURIOUS;
  else if (v < -0.4f && a < 0.2f)                             k = MOOD_LONELY;
  else if (a < -0.35f && v >= -0.4f)                          k = MOOD_BORED;
  else                                                        k = MOOD_CALM;
  kind = k;
}

// HSV → RGB, 0..255, for the LED policy (h in degrees).
static void hsv(float h, float s, float val, uint8_t* r, uint8_t* g, uint8_t* b) {
  float c = val * s, x = c * (1.0f - fabsf(fmodf(h / 60.0f, 2.0f) - 1.0f)), m = val - c;
  float rr = 0, gg = 0, bb = 0;
  if      (h <  60) { rr = c; gg = x; }
  else if (h < 120) { rr = x; gg = c; }
  else if (h < 180) { gg = c; bb = x; }
  else if (h < 240) { gg = x; bb = c; }
  else if (h < 300) { rr = x; bb = c; }
  else              { rr = c; bb = x; }
  *r = (uint8_t)((rr + m) * 255.0f + 0.5f);
  *g = (uint8_t)((gg + m) * 255.0f + 0.5f);
  *b = (uint8_t)((bb + m) * 255.0f + 0.5f);
}

void MoodEngine::express() {
  MoodExpr e = {};
  e.v = v; e.a = a; e.kind = kind;
  // Eyes: openness from arousal (Yamazaki: arousal-sleep axis = liveliness),
  // smile from valence, droop from low arousal or loneliness.
  e.openness = (uint8_t)moodClamp((0.55f + 0.4f * a) * 100.0f, 25.0f, 100.0f);
  e.happy = v > 0.3f;
  e.tired = (a < -0.4f) || kind == MOOD_LONELY;
  e.curious = kind == MOOD_CURIOUS || kind == MOOD_SURPRISED || kind == MOOD_AFFECTION;
  e.flicker = kind == MOOD_STARTLED;
  // Blink 1.5 s / (1 + A) → integer seconds; saccades: fast when keen, slow when flat.
  float blink = 1.5f / (1.0f + moodClamp(a, -0.5f, 1.0f)) + 1.0f;
  e.blinkSecs = (uint8_t)moodClamp(blink, 1.0f, 6.0f);
  e.saccadeSecs = (uint8_t)moodClamp(2.0f / (1.0f + 1.5f * moodClamp(a, -0.5f, 1.0f)) + 0.5f, 1.0f, 6.0f);
  // LED: hue from valence (red/orange −, white 0, green +; cyan for calm-positive), saturation from |V|,
  // brightness from arousal, pulse rate 0.25 / 0.5 / 2.5 Hz (MiRo).
  float hue = v >= 0 ? (a < 0 ? 170.0f : 120.0f) : 20.0f;    // calm+ → cyan, keen+ → green, − → orange
  if (kind == MOOD_AFFECTION) hue = 320.0f;                    // pink
  if (kind == MOOD_STARTLED)  hue = 0.0f;
  float sat = moodClamp(0.4f + 0.5f * fabsf(v), 0.0f, 1.0f);
  float bri = moodClamp(0.3f + 0.6f * (a + 1.0f) / 2.0f, 0.1f, 1.0f);
  hsv(hue, sat, bri, &e.r, &e.g, &e.b);
  e.pulseHz = a > 0.4f ? 2.5f : a < -0.3f ? 0.25f : 0.5f;
  // Head: tempo and amplitude with arousal; pitch bias down when low, up on surprise.
  e.tempo = moodClamp(1.0f + 0.6f * a, 0.5f, 1.6f);
  // Wander amplitude follows arousal across most of the neck's travel: bored
  // stares at its own desk, keen sweeps the room behind its shoulders.
  e.amplitude = (uint8_t)moodClamp(65.0f + 45.0f * a, 25.0f, 110.0f);
  e.pitchBias = kind == MOOD_SURPRISED || kind == MOOD_STARTLED ? 10
              : (kind == MOOD_LONELY || kind == MOOD_BORED) ? -10 : 0;
  // Sound: one chirp per kind change, rate-limited.
  e.chirp = MOODCHIRP_NONE;
  if (kind != lastKind && sinceChirpMs > CHIRP_COOLDOWN_MS) {
    switch (kind) {
      case MOOD_CURIOUS:   e.chirp = MOODCHIRP_CURIOUS; break;
      case MOOD_SURPRISED: e.chirp = MOODCHIRP_SURPRISE; break;
      case MOOD_STARTLED:  e.chirp = MOODCHIRP_STARTLE; break;
      case MOOD_BORED:
      case MOOD_LONELY:    e.chirp = MOODCHIRP_SIGH; break;
      case MOOD_AFFECTION:
      case MOOD_HAPPY:     e.chirp = MOODCHIRP_WARBLE; break;
      default: break;
    }
    if (e.chirp != MOODCHIRP_NONE) sinceChirpMs = 0;
  }
  lastKind = kind;
  switch (kind) {
    case MOOD_CURIOUS:   e.word = "curious..."; break;
    case MOOD_HAPPY:     e.word = "happy"; break;
    case MOOD_SURPRISED: e.word = "!"; break;
    case MOOD_STARTLED:  e.word = "eek!"; break;
    case MOOD_BORED:     e.word = "bored..."; break;
    case MOOD_LONELY:    e.word = "lonely..."; break;
    case MOOD_AFFECTION: e.word = "<3"; break;
    default:             e.word = "exploring..."; break;
  }
  expr = e;
}
