// Person tracking + owner memory glue. Includes M5StackChan.h (M5Unified's
// global ::M5); must never include board_compat.h — see hal_m5.h.
//
// Layers (from the bench-proven spike, now past-experiments/firmware/spikes/stackchan_look):
//   look.cpp      GC0308 on a core-0 task: motion centroid + skin-blob face
//   owner_model.h LIVE gaze (fresh target, EMA) + MEMORY (habit histogram)
//   this file     per-state policy -> bodyLookAt() / bodySearchSweep()
#include "ticks.h"
#include "gaze.h"
#include "body.h"
#include "hostlook.h"
#include "look.h"
#include "owner_model.h"
#include <M5StackChan.h>
#include <Preferences.h>

// ---- geometry (spike values; signs bench-verified 2026-09-27 with the camera) ----
static constexpr float kCameraHfovDeg = 66.0f;                 // GC0308 lens assumption
static constexpr float kCameraVfovDeg = kCameraHfovDeg * 3.0f / 4.0f;
// bench 2026-09-05: +1 — the head follows the hand (frame not mirrored
// relative to BSP yaw).
static constexpr float kYawSign  = 1.0f;
static constexpr float kElevSign = 1.0f;                       // bench 2026-09-27: +by (down) = lower pitch, with body.cpp's mirror
static constexpr float kYawLimitDeg = 60.0f;
static constexpr float kPitchMinDeg = 5.0f, kPitchMaxDeg = 85.0f;
static constexpr int   kObserveMinConf = 20;                   // weaker motion is noise
// Touch observation: same yaw the body turns to ("found you", bench-flipped
// sign: a toucher at the screen's right is at yaw -28).
static constexpr float kTouchYawDeg = -28.0f;
static constexpr float kPitchLevel  = 45.0f;

// ---- policy ----
static constexpr uint32_t kGentleIntervalMs = 3000;            // BUSY/IDLE glance rate
static constexpr float    kGentleDeadbandDeg = 8.0f;
static constexpr uint32_t kGentleHoldMs = 3000;
static constexpr uint32_t kWantHoldMs = 4000;                  // live hold in ATTENTION/LISTENING
static constexpr uint32_t kMemoryHoldMs = 12000;
static constexpr uint32_t kNotFoundMs = 5000;                  // live stale this long -> sweep
static constexpr uint32_t ATTN_SCAN_PERIOD_MS = 5000;          // sweep repeat while not found
static constexpr uint32_t kListenObsMs = 1000;                 // listening: strong obs cadence
static constexpr uint8_t  kListenObsWeight = 60;               // "3x" a typical camera obs (20)

// ---- persistence ----
static constexpr uint32_t kPeriodicSaveMs = 5UL * 60UL * 1000UL;
static constexpr uint32_t kMinSaveGapMs   = 60UL * 1000UL;
static constexpr int      kSaveConfCross  = 60;

static Preferences        prefs;
static owner::OwnerModel  model;
static look::Sample       sample = {};
static bool               cameraUp = false;
static bool               moving = false;
static uint32_t           lastServoPoll = 0;
static uint32_t           lastSaveMs = 0, lastPeriodicMs = 0;
static uint8_t            lastConf = 0;
static bool               wantPrev = false;
static uint32_t           lastLiveMs = 0;        // last fresh live target seen
static uint32_t           lastSweepMs = 0;
static uint32_t           lastGentleMs = 0;
static bool               searching = false;
static bool               locked = false;
static char               lastSrc = 'M';           // 'F' face blob, 'M' motion
static GazeObs            obs = {};                // latched for gazeTakeObs()
// Eye lead (gaze.h): the last reported face's offset from the frame centre, in degrees, and when.
static constexpr uint32_t kEyeLeadFreshMs = 700;   // ~3 frames at the host's rate; older than this is history
static constexpr float    kEyeLeadGain    = 2.0f;
static float              eyeLeadYaw = 0, eyeLeadPitch = 0;
static uint32_t           eyeLeadMs = 0;

static float clampf(float v, float lo, float hi) { return v < lo ? lo : (v > hi ? hi : v); }

static void saveModel(const char* why, uint32_t now) {
  uint8_t buf[owner::OwnerModel::kSerializedSize];
  size_t n = model.serialize(buf, sizeof(buf));
  if (n == 0) return;
  prefs.begin("owner", false);
  size_t w = prefs.putBytes("model", buf, n);
  prefs.end();
  lastSaveMs = now;
  owner::OwnerEstimate e = model.estimate();
  Serial.printf("[owner] saved %u bytes (%s) yaw=%.0f pitch=%.0f conf=%u\n",
                (unsigned)w, why, e.yawDeg, e.pitchDeg, e.conf);
}

static void restoreModel() {
  prefs.begin("owner", true);
  size_t n = prefs.getBytesLength("model");
  bool ok = false;
  if (n == owner::OwnerModel::kSerializedSize) {
    uint8_t buf[owner::OwnerModel::kSerializedSize];
    prefs.getBytes("model", buf, n);
    ok = model.deserialize(buf, n);
  }
  prefs.end();
  if (ok) {
    owner::OwnerEstimate e = model.estimate();
    Serial.printf("[owner] restored yaw=%.0f pitch=%.0f conf=%u\n", e.yawDeg, e.pitchDeg, e.conf);
  } else {
    Serial.println("[owner] fresh");
  }
}

void gazeBegin() {
  restoreModel();
  cameraUp = look::begin();      // false: memory + touch still work, no live layer
  lastPeriodicMs = millis();
  Serial.printf("[gaze] camera %s\n", cameraUp ? "up" : "OFF");
}

bool gazeTakeObs(GazeObs* out) {
  if (!obs.motionConf && !obs.faceSeen) return false;
  *out = obs;
  obs = GazeObs{};
  return true;
}

void gazeNoteTouch(int8_t side) {
  if (side == 0) return;
  model.observe(side * kTouchYawDeg, kPitchLevel, 2, millis());
}

// Issue a head move and tell both layers the head is in motion so the next
// camera frames are quarantined and no second move stacks on this one.
static void moveTo(float yaw, float pitch, uint32_t holdMs) {
  yaw = clampf(yaw, -kYawLimitDeg, kYawLimitDeg);
  pitch = clampf(pitch, kPitchMinDeg, kPitchMaxDeg);
  bodyLookAt((int8_t)yaw, (int8_t)pitch, (uint16_t)holdMs);
  look::setMoving(true);
  model.noteMoving(true);
  moving = true;
}

// ---- host vision ----
static constexpr uint32_t kHostFreshMs   = 3000;   // host faces within this: on-board target ignored
static constexpr uint8_t  kHostFaceMinConf = 40;
static uint32_t lastHostFaceSeq = 0;
static bool     hostLive = false;                  // host faces arriving
static float    hostFaceYaw = 0, hostFacePitch = 45;
static uint32_t hostFaceMs = 0;                    // last OWNER face with conf >= 40
static bool     hostFaceUnknownGlance = false;     // an unknown face wants a glance
static float    unkYaw = 0, unkPitch = 45; static uint32_t unkMs = 0;
static int8_t   lastBx = 0, lastBy = 0; static uint8_t lastSize = 0;

void gazeUpdate(PersonaState active, bool needsAttention, bool listening,
                uint32_t now, bool* ownerReset, GazeHostInput* host) {
  if (ownerReset && *ownerReset) {
    *ownerReset = false;
    model.reset();
    prefs.begin("owner", false); prefs.remove("model"); prefs.end();
    Serial.println("[owner] reset (host cmd)");
  }

  // Motion flag at 10 Hz: the body's tween OR the servo feedback (bench: no
  // bus warnings from isMoving() at this rate alongside the BSP task).
  if (now - lastServoPoll >= 100) {
    lastServoPoll = now;
    moving = bodyMoving();
    look::setMoving(moving);
    model.noteMoving(moving);
  }

  float curYaw = (float)bodyYawDeg(), curPitch = (float)bodyPitchDeg();

  // Frame stream to the host: on/off + fps from the cam cmd, paused while a
  // transfer owns the wire; every frame line carries the streamed pose.
  if (host) {
    look::setStream(host->camOn && cameraUp, host->camFps, host->camW, host->camH);
    look::setStreamPaused(host->wireBusy);
    look::setHeadPose(bodyCmdYawDeg(), bodyCmdPitchDeg());
  }

  // Host face detections (highest priority). Absolute angles use the pose
  // ECHOED in the face cmd (the pose at capture), never the current one.
  // elapsedMs: faceAtMs is stamped in dataPoll(), after `now` was read (ticks.h).
  bool hostFresh = host && host->faceAtMs && ticks::elapsedMs(now, host->faceAtMs) <= kHostFreshMs;
  if (hostFresh != hostLive) {
    hostLive = hostFresh;
    if (!hostLive) Serial.println("[gaze] host silent, on-board fallback");
  }
  if (host && host->faceAtMs && host->faceSeq != lastHostFaceSeq) {
    lastHostFaceSeq = host->faceSeq;
    if (host->faceConf >= kHostFaceMinConf) {
      float absYaw = host->faceYaw + kYawSign * (host->faceBx / 100.0f) * (kCameraHfovDeg * 0.5f);
      float absPitch = host->facePitch - kElevSign * (host->faceBy / 100.0f) * (kCameraVfovDeg * 0.5f); // +by = down
      absYaw = clampf(absYaw, -kYawLimitDeg, kYawLimitDeg);
      absPitch = clampf(absPitch, kPitchMinDeg, kPitchMaxDeg);
      lastBx = host->faceBx; lastBy = host->faceBy; lastSize = host->faceSize;
      eyeLeadYaw   =  kYawSign  * (host->faceBx / 100.0f) * (kCameraHfovDeg * 0.5f);
      eyeLeadPitch = -kElevSign * (host->faceBy / 100.0f) * (kCameraVfovDeg * 0.5f);   // +by = down
      eyeLeadMs    = now ? now : 1;
      obs.faceSeen = true; obs.faceOwner = obs.faceOwner || host->faceOwner;
      if (host->faceOwner) {
        // Owner: LIVE target + memory training.
        model.observe(absYaw, absPitch, host->faceConf, now);
        hostFaceYaw = absYaw; hostFacePitch = absPitch; hostFaceMs = now;
        lastSrc = 'H';
      } else {
        // Unknown face: a curious glance in BUSY/IDLE only, never trains memory.
        unkYaw = absYaw; unkPitch = absPitch; unkMs = now;
        hostFaceUnknownGlance = true;
      }
    }
  }

  // On-board observation -> absolute head-frame angles. Ignored while the
  // host is delivering faces.
  if (cameraUp && look::poll(&sample) && !sample.quarantined && !hostLive) {
    int b, e, conf; char src;
    if (look::bestTarget(sample, &b, &e, &conf, &src) && conf >= kObserveMinConf) {
      float absYaw = curYaw + kYawSign * (b / 100.0f) * (kCameraHfovDeg * 0.5f);
      float absPitch = curPitch + kElevSign * (e / 100.0f) * (kCameraVfovDeg * 0.5f);
      absYaw = clampf(absYaw, -kYawLimitDeg, kYawLimitDeg);
      absPitch = clampf(absPitch, kPitchMinDeg, kPitchMaxDeg);
      model.observe(absYaw, absPitch, (uint8_t)conf, now);
      lastSrc = src;
      if ((uint8_t)conf > obs.motionConf) obs.motionConf = (uint8_t)conf;
      if (src == 'F') obs.faceSeen = true;
    }
  }
  model.decay(now);

  // Host-driven gaze ({"cmd":"look"}): the rules live in hostlook.h — any state
  // during a voice conversation, calm states otherwise, never with a card up or
  // while dictating. The full neck range (±120), not the owner-tracking ±60.
  if (host && host->hostLookReq) {
    host->hostLookReq = false;
    bool ok = hostlook::accepted(host->cardUp, listening, host->agentActive,
                                 active == P_ATTENTION || needsAttention,
                                 active == P_SLEEP || active == P_IDLE || active == P_BUSY);
    if (ok) {
      int y = hostlook::clampYaw(host->hostLookYaw);
      int p = hostlook::clampPitch(host->hostLookPitch);
      bodyHostLook((int8_t)y, (int8_t)p, host->hostLookHold);
      look::setMoving(true); model.noteMoving(true); moving = true;
      Serial.printf("[gaze] host look yaw=%d pitch=%d hold=%u\n", y, p, (unsigned)host->hostLookHold);
    }
  }

  // Persistence: periodic + conf crossing 60, at most one write per 60 s.
  {
    uint8_t conf = model.estimate().conf;
    bool crossed = lastConf < kSaveConfCross && conf >= kSaveConfCross;
    lastConf = conf;
    bool periodic = now - lastPeriodicMs >= kPeriodicSaveMs;
    if ((periodic || crossed) && (lastSaveMs == 0 || now - lastSaveMs >= kMinSaveGapMs)) {
      lastPeriodicMs = now;
      saveModel(periodic ? "periodic" : "conf>=60", now);
    }
  }

  float ly = 0, lp = 0;
  bool live = model.liveTarget(now, &ly, &lp);
  if (live) lastLiveMs = now;

  bool want = active == P_ATTENTION || needsAttention || listening;
  if (want && !wantPrev) {
    // First move when the owner is wanted: the remembered spot.
    model.requestFind();
    lastSweepMs = now;               // give memory its 1.5 s before the first sweep
    searching = false; locked = false;
  }
  wantPrev = want;

  // Listening: the owner is at the laptop while the dictation key is held —
  // a strong observation at the current target teaches the habit fastest.
  static uint32_t lastListenObs = 0;
  if (listening && now - lastListenObs >= kListenObsMs) {
    lastListenObs = now;
    owner::OwnerEstimate e = model.estimate();
    float oy = live ? ly : (e.conf >= owner::kMinConfToMove ? e.yawDeg : curYaw);
    float op = live ? lp : (e.conf >= owner::kMinConfToMove ? e.pitchDeg : curPitch);
    model.observe(oy, op, kListenObsWeight, now);
  }

  float ty = 0, tp = 0;
  if (active == P_SLEEP || active == P_CELEBRATE || active == P_DIZZY || active == P_HEART) {
    if (!want) return;             // observe only; one-shots and sleep never move for the camera
  }

  if (want) {
    if (model.wantLiveMove(curYaw, curPitch, now, &ty, &tp)) {
      moveTo(ty, tp, kWantHoldMs);
      if (hostLive && lastSrc == 'H')
        Serial.printf("[gaze] host face bx=%d by=%d size=%u -> yaw=%d pitch=%d\n",
                      lastBx, lastBy, (unsigned)lastSize, (int)ty, (int)tp);
      else
        Serial.printf("[gaze] live yaw=%.0f pitch=%.0f conf=%u\n", ty, tp, model.liveConf());
      if (!locked) Serial.printf("[gaze] found src=%c\n", lastSrc);
      locked = true; searching = false;
      return;
    }
    if (live) { locked = true; searching = false; return; }   // in the deadband: hold
    locked = false;
    if (model.wantFallback(curYaw, curPitch, now, &ty, &tp)) {
      moveTo(ty, tp, kMemoryHoldMs);
      Serial.printf("[gaze] memory yaw=%.0f pitch=%.0f\n", ty, tp);
      return;
    }
    // Not found: live stale > 5 s -> sweep, again every ATTN_SCAN_PERIOD_MS
    // until live returns. Memory is re-armed after each sweep so the head
    // drifts back to the remembered spot in between.
    bool stale = lastLiveMs == 0 || now - lastLiveMs > kNotFoundMs;
    if (stale && !moving && now - lastSweepMs >= ATTN_SCAN_PERIOD_MS) {
      if (bodySearchSweep()) {
        if (!searching) Serial.println("[gaze] searching");
        searching = true;
        lastSweepMs = now;
        model.requestFind();
        look::setMoving(true); model.noteMoving(true); moving = true;
      }
    }
  } else {
    // BUSY / IDLE: a glance, not a stare. Live only, deadband 8 deg, 3 s.
    // An UNKNOWN host face gets the same glance and nothing more (it never
    // reaches the owner model).
    if (hostFaceUnknownGlance) {
      hostFaceUnknownGlance = false;
      bool far = fabsf(unkYaw - curYaw) >= kGentleDeadbandDeg || fabsf(unkPitch - curPitch) >= kGentleDeadbandDeg;
      if (far && !moving && now - unkMs < 1000 && now - lastGentleMs >= kGentleIntervalMs) {
        lastGentleMs = now;
        moveTo(unkYaw, unkPitch, kGentleHoldMs);
        Serial.printf("[gaze] host face (unknown) bx=%d by=%d size=%u -> yaw=%d pitch=%d (glance)\n",
                      lastBx, lastBy, (unsigned)lastSize, (int)unkYaw, (int)unkPitch);
      }
    } else if (model.wantLiveMove(curYaw, curPitch, now, &ty, &tp)) {
      bool far = fabsf(ty - curYaw) >= kGentleDeadbandDeg || fabsf(tp - curPitch) >= kGentleDeadbandDeg;
      if (far && now - lastGentleMs >= kGentleIntervalMs) {
        lastGentleMs = now;
        moveTo(ty, tp, kGentleHoldMs);
        Serial.printf("[gaze] live yaw=%.0f pitch=%.0f conf=%u (glance)\n", ty, tp, model.liveConf());
      }
    }
  }
}

int8_t gazeEyeLeadYawDeg() {
  if (!eyeLeadMs || ticks::elapsedMs(millis(), eyeLeadMs) > kEyeLeadFreshMs) return 0;
  return (int8_t)clampf(eyeLeadYaw * kEyeLeadGain, -60.0f, 60.0f);
}
int8_t gazeEyeLeadPitchDeg() {
  if (!eyeLeadMs || ticks::elapsedMs(millis(), eyeLeadMs) > kEyeLeadFreshMs) return 0;
  return (int8_t)clampf(eyeLeadPitch * kEyeLeadGain, -40.0f, 40.0f);
}
