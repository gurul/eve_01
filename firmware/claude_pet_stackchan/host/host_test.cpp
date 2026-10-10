// Host test for owner_model.h. Build from the repo root:
//   clang++ -std=c++17 -I firmware/claude_pet_stackchan/src \
//     firmware/claude_pet_stackchan/host/host_test.cpp -o /tmp/owner_test && /tmp/owner_test
#include "owner_model.h"

#include <cassert>
#include <cmath>
#include <cstdio>
#include <cstring>

using owner::OwnerEstimate;
using owner::OwnerModel;

// n observations 100 ms apart. Advances t.
static void feed(OwnerModel& m, float yaw, float pitch, int n, uint32_t& t, uint8_t w = 40) {
    for (int i = 0; i < n; ++i) {
        t += 100;
        m.observe(yaw, pitch, w, t);
    }
}

static bool near(float a, float b, float tol) { return std::fabs(a - b) < tol; }

static void test_deadband() {
    float y = 0, p = 0;
    // LIVE: 4 deg
    OwnerModel m;
    uint32_t t = 2000;
    feed(m, 3.0f, 45.0f, 3, t);
    assert(!m.wantLiveMove(0.0f, 45.0f, t, &y, &p));
    feed(m, 5.0f, 45.0f, 3, t);
    assert(m.wantLiveMove(0.0f, 45.0f, t, &y, &p));
    assert(near(y, 5.0f, 0.6f));
    // MEMORY fallback: 8 deg. Learn a spot, let live go stale, head 5 deg off.
    OwnerModel m2;
    t = 2000;
    feed(m2, 20.0f, 45.0f, 10, t);
    uint32_t stale = t + owner::kFallbackAfterStaleMs + 1;
    assert(!m2.wantFallback(15.0f, 45.0f, stale, &y, &p));   // inside deadband: hold
    OwnerModel m3;
    t = 2000;
    feed(m3, 20.0f, 45.0f, 10, t);
    stale = t + owner::kFallbackAfterStaleMs + 1;
    assert(m3.wantFallback(0.0f, 45.0f, stale, &y, &p));     // 20 deg off: move
    assert(near(y, 20.0f, 1.0f) && near(p, 45.0f, 1.0f));
    std::puts("deadband ok");
}

static void test_rate_limit() {
    float y = 0, p = 0;
    // LIVE: 600 ms between moves.
    OwnerModel m;
    uint32_t t = 2000;
    m.observe(30.0f, 45.0f, 50, t);
    assert(m.wantLiveMove(0.0f, 45.0f, t, &y, &p));
    m.observe(-30.0f, 45.0f, 50, t + 100);
    m.observe(-30.0f, 45.0f, 50, t + 200);
    assert(!m.wantLiveMove(30.0f, 45.0f, t + 200, &y, &p));  // too soon
    assert(!m.wantLiveMove(30.0f, 45.0f, t + 599, &y, &p));
    assert(m.wantLiveMove(30.0f, 45.0f, t + 600, &y, &p));   // allowed
    // MEMORY: fewer than 3 consistent observations teach nothing.
    OwnerModel m3;
    t = 2000;
    feed(m3, 30.0f, 45.0f, 2, t);
    assert(m3.estimate().conf == 0 && m3.binMass(9) == 0.0f);
    feed(m3, 30.0f, 45.0f, 1, t);
    assert(m3.estimate().conf > 0 && m3.binMass(9) > 0.0f);
    assert(near(m3.estimate().yawDeg, 30.0f, 0.1f));
    // A jump restarts the run.
    OwnerModel m4;
    t = 2000;
    m4.observe(30.0f, 45.0f, 40, t += 100);
    m4.observe(30.0f, 45.0f, 40, t += 100);
    m4.observe(-30.0f, 45.0f, 40, t += 100);
    assert(m4.estimate().conf == 0);
    // MEMORY: fallback waits 1.5 s after a live move.
    OwnerModel m5;
    t = 2000;
    feed(m5, 30.0f, 45.0f, 10, t);
    uint32_t stale = t + owner::kFallbackAfterStaleMs + 1;
    m5.observe(-30.0f, 45.0f, 50, stale);                    // live returns briefly
    assert(m5.wantLiveMove(30.0f, 45.0f, stale, &y, &p));
    uint32_t again = stale + owner::kFallbackAfterStaleMs + 1;
    assert(!m5.wantFallback(-30.0f, 45.0f, stale + 1000, &y, &p)); // live not stale yet anyway
    assert(m5.wantFallback(-30.0f, 45.0f, again, &y, &p));
    assert(near(y, 30.0f, 1.0f));
    std::puts("rate_limit ok");
}

static void test_ignore_while_moving() {
    OwnerModel m;
    uint32_t t = 2000;
    float y = 0, p = 0;
    m.noteMoving(true);
    feed(m, 40.0f, 45.0f, 20, t);
    assert(!m.liveTarget(t, &y, &p));
    assert(m.estimate().conf == 0);
    assert(!m.wantLiveMove(0.0f, 45.0f, t, &y, &p));
    assert(!m.wantFallback(0.0f, 45.0f, t, &y, &p));
    m.noteMoving(false);
    assert(m.estimate().conf == 0);            // nothing was learned while moving
    feed(m, 40.0f, 45.0f, 5, t);
    assert(m.wantLiveMove(0.0f, 45.0f, t, &y, &p));
    // a move in progress blocks both layers even with good targets
    feed(m, 40.0f, 45.0f, 5, t);
    m.noteMoving(true);
    assert(!m.wantLiveMove(0.0f, 45.0f, t + 700, &y, &p));
    assert(!m.wantFallback(0.0f, 45.0f, t + 20000, &y, &p));
    std::puts("ignore_while_moving ok");
}

static void test_histogram_prefers_habit() {
    OwnerModel m;
    uint32_t t = 2000;
    float y = 0, p = 0;
    feed(m, 30.0f, 50.0f, 300, t);             // 30 s at the desk
    feed(m, -40.0f, 40.0f, 8, t);              // someone walks past for 0.8 s
    OwnerEstimate e = m.estimate();
    assert(near(e.yawDeg, 30.0f, 2.0f));
    assert(near(e.pitchDeg, 50.0f, 2.0f));
    // Live would follow the passer-by, memory still says the desk.
    assert(m.liveTarget(t, &y, &p) && near(y, -40.0f, 1.0f));
    uint32_t stale = t + owner::kFallbackAfterStaleMs + 1;
    assert(m.wantFallback(-40.0f, 40.0f, stale, &y, &p));
    assert(near(y, 30.0f, 2.0f));
    // Sustained presence at the new spot eventually wins.
    t = stale;
    feed(m, -40.0f, 40.0f, 600, t);
    e = m.estimate();
    assert(near(e.yawDeg, -40.0f, 2.0f));
    // Decay lowers conf and mass when nothing is seen.
    uint8_t before = m.estimate().conf;
    m.decay(t + 3 * 60000);
    assert(m.estimate().conf < before);
    assert(m.binMass(2) < 6000.0f);
    std::puts("histogram_prefers_habit ok");
}

static void test_persistence() {
    OwnerModel m;
    uint32_t t = 2000;
    feed(m, 25.0f, 60.0f, 50, t);
    uint8_t buf[256];
    size_t n = m.serialize(buf, sizeof(buf));
    assert(n == OwnerModel::kSerializedSize);
    OwnerModel r;
    assert(r.deserialize(buf, n));
    OwnerEstimate a = m.estimate(), b = r.estimate();
    assert(near(a.yawDeg, b.yawDeg, 0.01f));
    assert(near(a.pitchDeg, b.pitchDeg, 0.01f));
    assert(a.conf == b.conf);
    assert(b.lastSeenMs == 0);                  // boot-relative, not persisted
    float y = 0, p = 0;
    // Boot: live never existed, so the fallback fires once toward the memory,
    // after the 1.5 s boot guard, and then holds.
    assert(!r.liveTarget(1000, &y, &p));
    assert(!r.wantFallback(0.0f, 45.0f, 1000, &y, &p));
    assert(r.wantFallback(0.0f, 45.0f, 2000, &y, &p));
    assert(near(y, 25.0f, 1.0f) && near(p, 60.0f, 1.0f));
    assert(!r.wantFallback(0.0f, 45.0f, 5000, &y, &p));
    r.requestFind();
    assert(r.wantFallback(0.0f, 45.0f, 5000, &y, &p));
    std::puts("persistence ok");
}

static void test_serialize_roundtrip() {
    OwnerModel m;
    uint32_t t = 2000;
    feed(m, -15.0f, 30.0f, 7, t);
    feed(m, 55.0f, 70.0f, 3, t);
    uint8_t a[256], b[256];
    size_t na = m.serialize(a, sizeof(a));
    assert(na > 0);
    OwnerModel r;
    assert(r.deserialize(a, na));
    size_t nb = r.serialize(b, sizeof(b));
    assert(na == nb);
    assert(std::memcmp(a, b, na) == 0);
    assert(m.serialize(a, 10) == 0);
    assert(!r.deserialize(a, 10));
    a[0] = 'X';
    assert(!r.deserialize(a, na));
    a[0] = 'O';
    a[3] = 99;                                  // unknown version
    assert(!r.deserialize(a, na));
    std::puts("serialize_roundtrip ok");
}

static void test_live_follows_fresh_motion() {
    OwnerModel m;
    float y = 0, p = 0;
    m.observe(30.0f, 50.0f, 45, 2000);
    assert(m.liveTarget(2000, &y, &p) && near(y, 30.0f, 0.01f) && near(p, 50.0f, 0.01f));
    assert(m.liveConf() == 45);
    assert(m.wantLiveMove(0.0f, 45.0f, 2000, &y, &p));
    assert(near(y, 30.0f, 0.01f));              // gain 1.0: one move lands on the target
    // EMA alpha 0.5 smooths the jumpy centroid.
    m.observe(10.0f, 50.0f, 45, 2100);
    assert(m.liveTarget(2100, &y, &p) && near(y, 20.0f, 0.01f));
    m.observe(10.0f, 50.0f, 45, 2200);
    assert(m.liveTarget(2200, &y, &p) && near(y, 15.0f, 0.01f));
    // Bench-like jitter: 26, 2, -4, 32, 25 at ~9 deg per 30 bearing -> still moves.
    OwnerModel m2;
    uint32_t t = 2000;
    const float seq[] = {8.6f, 0.7f, -1.3f, 10.6f, 8.3f};
    for (float v : seq) { t += 100; m2.observe(v, 45.0f, 50, t); }
    assert(m2.wantLiveMove(0.0f, 45.0f, t, &y, &p));
    assert(y > 4.0f);
    // A live observation does not by itself teach the memory (needs the run).
    OwnerModel m3;
    m3.observe(30.0f, 50.0f, 45, 2000);
    m3.observe(-30.0f, 50.0f, 45, 2100);
    assert(m3.estimate().conf == 0);
    std::puts("live_follows_fresh_motion ok");
}

static void test_live_ignored_when_stale() {
    OwnerModel m;
    float y = 0, p = 0;
    m.observe(30.0f, 50.0f, 45, 2000);
    assert(m.liveTarget(4000, &y, &p));         // exactly 2000 ms: still fresh
    assert(!m.liveTarget(4001, &y, &p));
    assert(!m.wantLiveMove(0.0f, 45.0f, 4001, &y, &p));
    // A weak observation does not revive the live target.
    m.observe(30.0f, 50.0f, 15, 4100);
    assert(!m.liveTarget(4100, &y, &p));
    // A strong one after staleness restarts the EMA from scratch (no blend with old).
    m.observe(-30.0f, 50.0f, 45, 4200);
    assert(m.liveTarget(4200, &y, &p) && near(y, -30.0f, 0.01f));
    std::puts("live_ignored_when_stale ok");
}

static void test_fallback_after_stale() {
    OwnerModel m;
    uint32_t t = 2000;
    float y = 0, p = 0;
    feed(m, 30.0f, 50.0f, 100, t);             // habit at 30/50, head is there
    feed(m, -20.0f, 40.0f, 5, t);              // owner briefly at -20
    assert(m.wantLiveMove(30.0f, 50.0f, t, &y, &p) && near(y, -20.0f, 2.0f)); // EMA after 5 obs
    // head now at -20. Nothing seen after t.
    assert(!m.wantFallback(-20.0f, 40.0f, t + 3000, &y, &p));   // stale < 8 s
    assert(!m.wantFallback(-20.0f, 40.0f, t + 8000, &y, &p));   // exactly 8 s: not yet
    assert(m.wantFallback(-20.0f, 40.0f, t + 8001, &y, &p));    // drift back once
    assert(near(y, 30.0f, 2.0f) && near(p, 50.0f, 2.0f));
    assert(!m.wantFallback(-20.0f, 40.0f, t + 12000, &y, &p));  // then hold
    assert(!m.wantFallback(-20.0f, 40.0f, t + 60000, &y, &p));
    // Live comes back, then goes stale again: fallback re-arms by itself.
    m.observe(-20.0f, 40.0f, 45, t + 61000);
    assert(!m.wantFallback(-20.0f, 40.0f, t + 62000, &y, &p));
    assert(m.wantFallback(-20.0f, 40.0f, t + 61000 + 8001, &y, &p));
    // Fresh model with no memory never falls back.
    OwnerModel e;
    assert(!e.wantFallback(0.0f, 45.0f, 20000, &y, &p));
    std::puts("fallback_after_stale ok");
}

int main() {
    test_deadband();
    test_persistence();
    test_rate_limit();
    test_ignore_while_moving();
    test_histogram_prefers_habit();
    test_serialize_roundtrip();
    test_live_follows_fresh_motion();
    test_live_ignored_when_stale();
    test_fallback_after_stale();
    std::puts("owner_model: all tests passed");
    return 0;
}
