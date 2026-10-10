#pragma once
// The pet's feelings while it explores: a small affect engine.
//
// Model (see docs/stackchan/personality.md for the papers behind it):
//   - valence V and arousal A in [-1, 1]: the fast emotion. Stimuli bump
//     them; they relax toward a slow mood baseline with tau ~60 s (Mini,
//     Fernández-Rodicio 2022: k=0.05).
//   - moodV / moodA: the slow baseline, tau ~1 h, drifting toward the
//     running emotion (k=0.001).
//   - two drives in [0, 1] with a 0.5 set-point (Kismet / Vector axes):
//     social (rises alone, satisfied by a face or a touch) and stimulation
//     (rises when nothing new happens, satisfied by motion / a new view).
//     An unsatisfied drive colours V/A: bored (stimulation high) drags A
//     down, lonely (social high) drags V down.
//   - a startle detector: a sudden big motion while calm.
// Expression is a modulation of parameters read off (V, A) plus a discrete
// kind for the few things that are clips (a startle jerk, a curious tilt),
// following MiRo / Nutty: eyes, LED colour and pulse, head tempo, chirp.
//
// Pure C++: no Arduino, no randomness, no clock — the caller passes dt.
#include <stdint.h>

enum MoodKind : uint8_t {
  MOOD_CALM = 0,
  MOOD_CURIOUS,
  MOOD_HAPPY,
  MOOD_SURPRISED,
  MOOD_STARTLED,
  MOOD_BORED,
  MOOD_LONELY,
  MOOD_AFFECTION,
};

// What happened since the last step. Booleans are edge events for this step
// (the caller latches them), except `exploring`/`asleep` which are levels.
struct MoodInput {
  bool    exploring;      // explore mode: the engine only expresses here, but always integrates
  bool    asleep;         // persona SLEEP: everything relaxes, no startle
  uint8_t motionConf;     // 0..100 on-board motion/skin confidence this step (0 = nothing)
  bool    faceSeen;       // a face (host or on-board) this step
  bool    faceOwner;      // ... and it is the owner
  bool    touched;        // a touch on the pet this step
  bool    newView;        // the head arrived somewhere it has not looked in a while
  bool    hostEmote;      // {"cmd":"emote"} arrived: apply dv/da (each -100..100 → ±1.0, clamped ±0.3)
  int8_t  dv, da;
};

// Continuous expression parameters (all derived from V/A/kind each step).
struct MoodExpr {
  float    v, a;              // for logging / the status line
  MoodKind kind;
  // eyes
  uint8_t  openness;          // 0..100: eyelid openness (RoboEyes height scale)
  bool     happy;             // HAPPY mood (lower-lid smile)
  bool     tired;             // TIRED mood (droopy lids)
  bool     curious;           // setCuriosity
  bool     flicker;           // horizontal flicker (startle)
  uint8_t  blinkSecs;         // autoblinker interval, 1..6
  uint8_t  saccadeSecs;       // idle-mode interval, 1..6 (gaze shift tempo)
  // LED
  uint8_t  r, g, b;           // colour at full pulse
  float    pulseHz;           // 0.25 calm .. 2.5 excited
  // head
  float    tempo;             // wander speed multiplier 0.5..1.6 (period /= tempo)
  int8_t   pitchBias;         // degrees added to the wander pitch (down = sad/bored, up = surprise)
  uint8_t  amplitude;         // wander yaw amplitude 25..110 (neck allows ±120)
  // sound / word
  uint8_t  chirp;             // MoodChirp below, or MOODCHIRP_NONE
  const char* word;           // status row: "curious...", "!", "bored...", ...
};

enum MoodChirp : uint8_t { MOODCHIRP_NONE = 0, MOODCHIRP_CURIOUS, MOODCHIRP_SURPRISE, MOODCHIRP_SIGH,
                           MOODCHIRP_WARBLE, MOODCHIRP_STARTLE };

struct MoodEngine {
  // state
  float v = 0.0f, a = 0.0f;
  float moodV = 0.05f, moodA = 0.0f;   // a slightly cheerful baseline: personality
  float social = 0.5f, stimulation = 0.5f;
  uint32_t sinceFaceMs = 600000, sinceMotionMs = 600000, sinceTouchMs = 600000;
  uint32_t sinceOwnerMs = 600000, sinceSurpriseMs = 600000, sinceStartleMs = 600000;
  uint32_t sinceChirpMs = 600000;
  MoodKind kind = MOOD_CALM;
  MoodKind lastKind = MOOD_CALM;   // for the one-chirp-per-change rule
  MoodExpr expr = {};

  void reset();
  // Integrate one step of dtMs and refresh `expr`. Returns the kind.
  MoodKind step(const MoodInput& in, uint32_t dtMs);
  // Constants (tuned from the literature, see the header comment)
  static constexpr float TAU_EMOTION_S = 60.0f;
  static constexpr float TAU_MOOD_S    = 3600.0f;
  static constexpr float SOCIAL_RISE_PER_S = 0.0004f;    // alone: ~8 min to the lonely regime (0.7), ~17 min to 0.9
  static constexpr float STIM_RISE_PER_S   = 0.001f;     // nothing new: ~2 min to the bored regime (0.6)
  static constexpr float HOST_EMOTE_CLAMP  = 0.3f;
  static constexpr uint32_t STARTLE_COOLDOWN_MS = 20000;
  static constexpr uint32_t CHIRP_COOLDOWN_MS   = 8000;

private:
  void classify();
  void express();
};

// Helpers shared with the reference model (kept tiny and explicit).
float moodClamp(float x, float lo, float hi);
