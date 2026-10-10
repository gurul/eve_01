// owner_model.h — two layers: LIVE gaze (follow fresh motion now) and MEMORY
// (where the laptop owner usually is; the pose to fall back to).
// Pure C++17. No Arduino includes. Header-only so the host test and the
// sketch compile the same code.
//
// Angles are absolute head-frame degrees: yaw positive = the head turns to
// its own left (StackChan-BSP convention), pitch 0..90 (5..85 usable).
#pragma once

#include <cmath>
#include <cstddef>
#include <cstdint>
#include <cstring>

namespace owner {

// ---- LIVE gaze (follows fresh motion) -------------------------------------
constexpr uint8_t kLiveMinConf = 20;          // weaker observations do not steer the gaze
constexpr float kLiveAlpha = 0.5f;            // EMA weight of a new observation
constexpr uint32_t kLiveFreshMs = 2000;       // live target valid this long after the last obs
constexpr float kLiveDeadbandDeg = 4.0f;      // no live move when target is this close
constexpr uint32_t kLiveMoveIntervalMs = 600; // rate limit between live moves

// ---- MEMORY (habitual spot, fallback pose) --------------------------------
constexpr float kDeadbandDeg = 8.0f;          // no fallback move when target is this close
constexpr int kMinConsistentObs = 3;          // consistent obs before memory learns a spot
constexpr float kConsistencyDeg = 10.0f;      // "consistent" = within this of the anchor
constexpr uint32_t kMinMoveIntervalMs = 1500; // fallback waits this long after any move
constexpr uint32_t kFallbackAfterStaleMs = 8000; // live stale this long -> drift back once
constexpr uint8_t kMinConfToMove = 10;        // below this the memory is a rumour

// ---- histogram / memory constants ----------------------------------------
constexpr int kBins = 12;
constexpr float kYawMinDeg = -60.0f;
constexpr float kYawMaxDeg = 60.0f;
constexpr float kBinWidthDeg = (kYawMaxDeg - kYawMinDeg) / kBins; // 10 deg
constexpr float kMassCap = 6000.0f;           // total histogram mass is normalised to this
constexpr float kMassDecayPerMin = 0.97f;     // habit fades slowly
constexpr uint32_t kDecayStepMs = 60000;      // decay() acts once per minute
constexpr uint8_t kConfDecayPerMin = 10;      // conf falls 10 points per idle minute

struct OwnerEstimate {
    float yawDeg;
    float pitchDeg;
    uint8_t conf;        // 0..100
    uint32_t lastSeenMs; // 0 = never seen this boot
};

class OwnerModel {
public:
    OwnerModel() { reset(); }

    void reset() {
        for (int i = 0; i < kBins; ++i) {
            mass_[i] = 0.0f;
            yawSum_[i] = 0.0f;
            pitchSum_[i] = 0.0f;
        }
        conf_ = 0;
        lastSeenMs_ = 0;
        lastMoveMs_ = 0;
        lastDecayMs_ = 0;
        moving_ = false;
        consistent_ = 0;
        anchorYaw_ = 0.0f;
        anchorPitch_ = 0.0f;
        liveValid_ = false;
        liveYaw_ = 0.0f;
        livePitch_ = 45.0f;
        liveConf_ = 0;
        liveLastMs_ = 0;
        lastLiveMoveMs_ = 0;
        fallbackDone_ = false;
    }

    // Feed one observation. weight 0..100 (the perception confidence).
    // Observations while noteMoving(true) is in force are dropped.
    // LIVE: EMA of fresh observations with weight >= kLiveMinConf.
    // MEMORY: an observation reaches the histogram only as part of a run of
    // >= kMinConsistentObs observations within kConsistencyDeg of each other.
    void observe(float absYawDeg, float absPitchDeg, uint8_t weight, uint32_t nowMs) {
        if (moving_ || weight == 0) return;
        if (absYawDeg < kYawMinDeg || absYawDeg > kYawMaxDeg) return;

        lastSeenMs_ = nowMs;
        if (lastDecayMs_ == 0) lastDecayMs_ = nowMs;

        if (weight >= kLiveMinConf) {
            bool fresh = liveValid_ && (nowMs - liveLastMs_) <= kLiveFreshMs;
            if (!fresh) {
                liveYaw_ = absYawDeg;
                livePitch_ = absPitchDeg;
            } else {
                liveYaw_ += (absYawDeg - liveYaw_) * kLiveAlpha;
                livePitch_ += (absPitchDeg - livePitch_) * kLiveAlpha;
            }
            liveConf_ = weight;
            liveLastMs_ = nowMs;
            liveValid_ = true;
            fallbackDone_ = false; // live is back: the next stale period may fall back again
        }

        // consistency run for the memory layer
        if (consistent_ == 0 ||
            std::fabs(absYawDeg - anchorYaw_) > kConsistencyDeg ||
            std::fabs(absPitchDeg - anchorPitch_) > kConsistencyDeg) {
            anchorYaw_ = absYawDeg;
            anchorPitch_ = absPitchDeg;
            consistent_ = 1;
            pendYaw_[0] = absYawDeg; pendPitch_[0] = absPitchDeg; pendW_[0] = weight;
            return;
        }
        if (consistent_ < kMinConsistentObs) {
            pendYaw_[consistent_] = absYawDeg;
            pendPitch_[consistent_] = absPitchDeg;
            pendW_[consistent_] = weight;
            ++consistent_;
            if (consistent_ == kMinConsistentObs) {
                for (int i = 0; i < kMinConsistentObs; ++i) commit(pendYaw_[i], pendPitch_[i], pendW_[i]);
            }
            return;
        }
        if (consistent_ < 255) ++consistent_;
        commit(absYawDeg, absPitchDeg, weight);
    }

    // Re-arm the memory fallback ("find the owner"). Also the boot state.
    void requestFind() { fallbackDone_ = false; }

    void noteMoving(bool moving) {
        moving_ = moving;
        if (moving) consistent_ = 0; // a move invalidates the consistency run
    }

    // LIVE target: valid only while an observation arrived within kLiveFreshMs.
    bool liveTarget(uint32_t nowMs, float* yaw, float* pitch) const {
        if (!liveValid_ || (nowMs - liveLastMs_) > kLiveFreshMs) return false;
        if (yaw) *yaw = liveYaw_;
        if (pitch) *pitch = livePitch_;
        return true;
    }
    uint8_t liveConf() const { return liveConf_; }

    // LIVE move: follow the fresh target. Deadband 4 deg, min 600 ms between
    // moves, nothing while moving. One move lands the target at frame centre.
    bool wantLiveMove(float curYaw, float curPitch, uint32_t nowMs, float* outYaw, float* outPitch) {
        if (moving_) return false;
        float y, p;
        if (!liveTarget(nowMs, &y, &p)) return false;
        if (lastLiveMoveMs_ != 0 && nowMs - lastLiveMoveMs_ < kLiveMoveIntervalMs) return false;
        if (std::fabs(y - curYaw) < kLiveDeadbandDeg && std::fabs(p - curPitch) < kLiveDeadbandDeg) return false;
        if (outYaw) *outYaw = y;
        if (outPitch) *outPitch = p;
        lastLiveMoveMs_ = nowMs;
        return true;
    }

    // MEMORY fallback: once live has been stale for kFallbackAfterStaleMs (or
    // never existed: boot), drift to the remembered spot one time, then hold.
    // requestFind() re-arms it. Deadband 8 deg; waits 1.5 s after any move.
    bool wantFallback(float curYaw, float curPitch, uint32_t nowMs, float* outYaw, float* outPitch) {
        if (moving_ || fallbackDone_) return false;
        if (conf_ < kMinConfToMove) return false;
        if (liveValid_ && (nowMs - liveLastMs_) <= kFallbackAfterStaleMs) return false;
        if (nowMs < kMinMoveIntervalMs) return false; // never move in the first 1.5 s
        if (lastMoveMs_ != 0 && nowMs - lastMoveMs_ < kMinMoveIntervalMs) return false;
        if (lastLiveMoveMs_ != 0 && nowMs - lastLiveMoveMs_ < kMinMoveIntervalMs) return false;

        OwnerEstimate e = estimate();
        fallbackDone_ = true; // fire (or hold) once per stale period
        if (std::fabs(e.yawDeg - curYaw) < kDeadbandDeg &&
            std::fabs(e.pitchDeg - curPitch) < kDeadbandDeg) {
            return false;
        }
        if (outYaw) *outYaw = e.yawDeg;
        if (outPitch) *outPitch = e.pitchDeg;
        lastMoveMs_ = nowMs;
        return true;
    }

    // Peak histogram bin refined with its neighbours. The habitual spot wins.
    OwnerEstimate estimate() const {
        OwnerEstimate e{0.0f, 45.0f, conf_, lastSeenMs_};
        int peak = -1;
        float best = 0.0f;
        for (int i = 0; i < kBins; ++i) {
            if (mass_[i] > best) { best = mass_[i]; peak = i; }
        }
        if (peak < 0) return e;
        float m = 0.0f, ys = 0.0f, ps = 0.0f;
        for (int i = peak - 1; i <= peak + 1; ++i) {
            if (i < 0 || i >= kBins) continue;
            m += mass_[i];
            ys += yawSum_[i];
            ps += pitchSum_[i];
        }
        if (m > 0.0f) {
            e.yawDeg = ys / m;
            e.pitchDeg = ps / m;
        }
        return e;
    }

    // Call regularly. Conf and histogram fade when nothing is seen.
    void decay(uint32_t nowMs) {
        if (lastDecayMs_ == 0) { lastDecayMs_ = nowMs; return; }
        while (nowMs - lastDecayMs_ >= kDecayStepMs) {
            lastDecayMs_ += kDecayStepMs;
            if (lastSeenMs_ == 0 || nowMs - lastSeenMs_ >= kDecayStepMs) {
                conf_ = conf_ > kConfDecayPerMin ? conf_ - kConfDecayPerMin : 0;
            }
            for (int i = 0; i < kBins; ++i) {
                mass_[i] *= kMassDecayPerMin;
                yawSum_[i] *= kMassDecayPerMin;
                pitchSum_[i] *= kMassDecayPerMin;
            }
        }
    }

    float binMass(int i) const { return (i >= 0 && i < kBins) ? mass_[i] : 0.0f; }
    uint8_t consistentCount() const { return consistent_; }

    // ---- persistence: little-endian raw floats, magic + version header ----
    static constexpr size_t kSerializedSize = 4 + kBins * 3 * sizeof(float) + 1;

    size_t serialize(uint8_t* buf, size_t cap) const {
        if (!buf || cap < kSerializedSize) return 0;
        size_t o = 0;
        buf[o++] = 'O'; buf[o++] = 'W'; buf[o++] = 'N'; buf[o++] = kVersion;
        for (int i = 0; i < kBins; ++i) { std::memcpy(buf + o, &mass_[i], 4); o += 4; }
        for (int i = 0; i < kBins; ++i) { std::memcpy(buf + o, &yawSum_[i], 4); o += 4; }
        for (int i = 0; i < kBins; ++i) { std::memcpy(buf + o, &pitchSum_[i], 4); o += 4; }
        buf[o++] = conf_;
        return o;
    }

    bool deserialize(const uint8_t* buf, size_t len) {
        if (!buf || len < kSerializedSize) return false;
        if (buf[0] != 'O' || buf[1] != 'W' || buf[2] != 'N' || buf[3] != kVersion) return false;
        float mass[kBins], ys[kBins], ps[kBins];
        size_t o = 4;
        for (int i = 0; i < kBins; ++i) { std::memcpy(&mass[i], buf + o, 4); o += 4; }
        for (int i = 0; i < kBins; ++i) { std::memcpy(&ys[i], buf + o, 4); o += 4; }
        for (int i = 0; i < kBins; ++i) { std::memcpy(&ps[i], buf + o, 4); o += 4; }
        for (int i = 0; i < kBins; ++i) {
            if (!std::isfinite(mass[i]) || mass[i] < 0.0f) return false;
            if (!std::isfinite(ys[i]) || !std::isfinite(ps[i])) return false;
        }
        uint8_t conf = buf[o++];
        if (conf > 100) return false;
        reset();
        for (int i = 0; i < kBins; ++i) { mass_[i] = mass[i]; yawSum_[i] = ys[i]; pitchSum_[i] = ps[i]; }
        conf_ = conf;
        return true;
    }

private:
    static constexpr uint8_t kVersion = 1;

    static int binFor(float yaw) {
        int b = static_cast<int>((yaw - kYawMinDeg) / kBinWidthDeg);
        if (b < 0) b = 0;
        if (b >= kBins) b = kBins - 1;
        return b;
    }

    void commit(float yaw, float pitch, uint8_t weight) {
        int bin = binFor(yaw);
        float w = static_cast<float>(weight);
        mass_[bin] += w;
        yawSum_[bin] += w * yaw;
        pitchSum_[bin] += w * pitch;
        normalise();
        int c = conf_ + (weight >= 8 ? weight / 8 : 1);
        conf_ = static_cast<uint8_t>(c > 100 ? 100 : c);
    }

    void normalise() {
        float total = 0.0f;
        for (int i = 0; i < kBins; ++i) total += mass_[i];
        if (total <= kMassCap) return;
        float k = kMassCap / total;
        for (int i = 0; i < kBins; ++i) {
            mass_[i] *= k;
            yawSum_[i] *= k;
            pitchSum_[i] *= k;
        }
    }

    float mass_[kBins];
    float yawSum_[kBins];
    float pitchSum_[kBins];
    uint8_t conf_;
    uint32_t lastSeenMs_;
    uint32_t lastMoveMs_;
    uint32_t lastDecayMs_;
    bool moving_;
    uint8_t consistent_;
    float anchorYaw_;
    float anchorPitch_;
    float pendYaw_[kMinConsistentObs];
    float pendPitch_[kMinConsistentObs];
    uint8_t pendW_[kMinConsistentObs];

    bool liveValid_;
    float liveYaw_;
    float livePitch_;
    uint8_t liveConf_;
    uint32_t liveLastMs_;
    uint32_t lastLiveMoveMs_;
    bool fallbackDone_;
};

} // namespace owner
