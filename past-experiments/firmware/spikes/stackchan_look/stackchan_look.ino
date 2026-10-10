// stackchan_look — spike: find the person by camera motion, remember where the
// owner usually sits, turn the head toward them without twitching.
//
// Board: M5StackChan K151 (CoreS3). FQBN:
//   esp32:esp32:m5stack_cores3:PartitionScheme=huge_app,PSRAM=enabled
// Two layers: LIVE gaze follows fresh motion (deadband 4 deg, 600 ms between
// moves); MEMORY remembers the habitual spot and pulls the head back once
// after live has been stale for 8 s.
// Perception: skin-colour face blob fused with the motion centroid (face wins
// when face_conf >= 25, else motion when conf >= 20).
// Serial (115200): f = toggle follow, r = reset model, p = print both layers,
//                  s = save model to NVS now, o = find the owner (re-arm fallback),
//                  t = skin/blob stats + decode proof (centre RGB/YCbCr, Cr range),
//                  b = flip the RGB565 byte order live, c = dump the 80x60 cell
//                  grid (current order), C = dump with the opposite order,
//                  w = toggle AWB.
#include <M5StackChan.h>
#include <Preferences.h>

#include "src/look.h"
#include "src/owner_model.h"

// ---- geometry --------------------------------------------------------------
static constexpr float kCameraHfovDeg = 66.0f;                 // GC0308 lens assumption; live gain is 1.0
static constexpr float kCameraVfovDeg = kCameraHfovDeg * 3.0f / 4.0f;
// BSP: yaw +  = head turns to its own left (moveX(+1000) is "Turn Left").
// look: bearing + = subject on the right of the frame. So a subject on the
// right needs a smaller yaw. Flip on the bench if the head runs away.
static constexpr float kYawSign = 1.0f;   // bench 2026-09-05: -1 turned the head away from the waver — the GC0308 frame is not mirrored relative to BSP yaw
static constexpr float kElevSign = 1.0f;
static constexpr float kYawLimitDeg = 60.0f;
static constexpr float kPitchMinDeg = 5.0f;
static constexpr float kPitchMaxDeg = 85.0f;
static constexpr int kMoveSpeed = 300;                          // 0..1000, moderate

// ---- persistence -----------------------------------------------------------
static constexpr uint32_t kPeriodicSaveMs = 5UL * 60UL * 1000UL;
static constexpr uint32_t kMinSaveGapMs = 60UL * 1000UL;
static constexpr int kSaveConfCross = 60;

static Preferences g_prefs;
static owner::OwnerModel g_model;
static bool g_follow = true;
static uint32_t g_lastSaveMs = 0;
static uint32_t g_lastPeriodicMs = 0;
static int g_lastConf = 0;
static float g_curYaw = 0.0f, g_curPitch = 45.0f;
static bool g_moving = false;
static look::Sample g_sample = {};
static int g_tBearing = 0, g_tElev = 0, g_tConf = 0;   // fused target
static char g_tSource = 0;                             // 'F', 'M', or 0

static float clampf(float v, float lo, float hi) { return v < lo ? lo : (v > hi ? hi : v); }

static void saveModel(const char* why) {
    uint8_t buf[owner::OwnerModel::kSerializedSize];
    size_t n = g_model.serialize(buf, sizeof(buf));
    if (n == 0) return;
    g_prefs.begin("owner", false);
    size_t w = g_prefs.putBytes("model", buf, n);
    g_prefs.end();
    g_lastSaveMs = millis();
    owner::OwnerEstimate e = g_model.estimate();
    Serial.printf("[owner] saved %u bytes (%s) yaw=%.0f pitch=%.0f conf=%u\n",
                  (unsigned)w, why, e.yawDeg, e.pitchDeg, e.conf);
}

static void restoreModel() {
    g_prefs.begin("owner", true);
    size_t n = g_prefs.getBytesLength("model");
    bool ok = false;
    if (n == owner::OwnerModel::kSerializedSize) {
        uint8_t buf[owner::OwnerModel::kSerializedSize];
        g_prefs.getBytes("model", buf, n);
        ok = g_model.deserialize(buf, n);
    }
    g_prefs.end();
    if (ok) {
        owner::OwnerEstimate e = g_model.estimate();
        Serial.printf("[owner] restored yaw=%.0f pitch=%.0f conf=%u\n", e.yawDeg, e.pitchDeg, e.conf);
    } else {
        Serial.println("[owner] fresh");
    }
}

static void printEstimate() {
    uint32_t now = millis();
    float ly = 0, lp = 0;
    if (g_model.liveTarget(now, &ly, &lp)) {
        Serial.printf("[live] yaw=%.1f pitch=%.1f conf=%u (fresh)\n", ly, lp, g_model.liveConf());
    } else {
        Serial.println("[live] stale");
    }
    owner::OwnerEstimate e = g_model.estimate();
    Serial.printf("[memory] yaw=%.1f pitch=%.1f conf=%u lastSeen=%lu consistent=%u head=%.0f/%.0f follow=%d\n",
                  e.yawDeg, e.pitchDeg, e.conf, (unsigned long)e.lastSeenMs,
                  g_model.consistentCount(), g_curYaw, g_curPitch, g_follow ? 1 : 0);
}

static void handleSerial() {
    while (Serial.available()) {
        int c = Serial.read();
        switch (c) {
            case 'f':
                g_follow = !g_follow;
                Serial.printf("[cmd] follow=%d\n", g_follow ? 1 : 0);
                break;
            case 'r':
                g_model.reset();
                Serial.println("[cmd] model reset");
                break;
            case 'p':
                printEstimate();
                break;
            case 's':
                saveModel("cmd");
                break;
            case 'o':
                g_model.requestFind();
                Serial.println("[cmd] find owner: fallback re-armed");
                break;
            case 't': {
                look::Stats st;
                look::stats(&st);
                Serial.printf("[tune] skin=%d/1000 cells blobs=%d largest=%d cells (%dx%d) picked=%d cells "
                              "face_conf=%d cost=%luus bounds Cb[%d,%d] Cr[%d,%d] Y>=%d\n",
                              st.skinCellsPerMille, st.blobCount, st.largestArea, st.largestW, st.largestH,
                              st.pickedArea, g_sample.face_conf, (unsigned long)st.costUs,
                              look::kSkinCbMin, look::kSkinCbMax, look::kSkinCrMin, look::kSkinCrMax, look::kSkinYMin);
                Serial.printf("[tune] centre8x8 R=%d G=%d B=%d Y=%d Cb=%d Cr=%d | grid Cr min=%d max=%d | byteswap=%d\n",
                              st.cR, st.cG, st.cB, st.cY, st.cCb, st.cCr, st.crMin, st.crMax, (int)st.byteSwap);
                look::printSensor();
                break;
            }
            case 'c':
                look::requestDump(false);
                break;
            case 'C':
                look::requestDump(true);
                break;
            case 'w':
                look::toggleAwb();
                break;
            case 'b':
                look::setByteSwap(!look::byteSwap());
                Serial.printf("[cmd] rgb565 byteswap=%d (press t after a frame)\n", (int)look::byteSwap());
                break;
            default:
                break;
        }
    }
}

static void drawOverlay() {
    auto& d = M5.Display;
    owner::OwnerEstimate e = g_model.estimate();
    d.setTextDatum(top_left);
    d.setTextSize(2);
    d.fillRect(0, 0, 320, 24, TFT_BLACK);
    d.setCursor(4, 4);
    d.printf("F%3d,%3d c%2d M%3d,%3d c%2d", g_sample.face_bearing, g_sample.face_elev, g_sample.face_conf,
             g_sample.bearing, g_sample.elev, g_sample.conf);

    // bearing bar
    d.fillRect(0, 40, 320, 30, TFT_BLACK);
    d.drawRect(10, 45, 300, 20, TFT_DARKGREY);
    d.drawFastVLine(160, 45, 20, TFT_DARKGREY);
    int bx = 160 + g_tBearing * 150 / 100;
    uint16_t col = g_sample.quarantined ? TFT_DARKGREY
                 : (g_tSource == 'F' ? TFT_GREEN : (g_tSource == 'M' ? TFT_YELLOW : TFT_DARKGREY));
    d.fillRect(bx - 4, 46, 8, 18, col);

    // frame view (1 px per skin cell, 80x60) bottom-right: face box + fused dot
    const int fx = 236, fy = 176;
    d.fillRect(fx - 1, fy - 1, look::kSkinW + 2, look::kSkinH + 2, TFT_BLACK);
    d.drawRect(fx - 1, fy - 1, look::kSkinW + 2, look::kSkinH + 2, TFT_DARKGREY);
    if (g_sample.face_seen) {
        d.drawRect(fx + g_sample.face_x0, fy + g_sample.face_y0,
                   g_sample.face_x1 - g_sample.face_x0 + 1, g_sample.face_y1 - g_sample.face_y0 + 1, TFT_GREEN);
    }
    if (g_tSource) {
        int px = fx + (g_tBearing + 100) * (look::kSkinW - 1) / 200;
        int py = fy + (100 - g_tElev) * (look::kSkinH - 1) / 200;
        d.fillCircle(px, py, 2, g_tSource == 'F' ? TFT_GREEN : TFT_YELLOW);
    }

    // head + estimate
    d.fillRect(0, 90, 320, 60, TFT_BLACK);
    d.setCursor(4, 92);
    d.printf("head y%4.0f p%3.0f %s", g_curYaw, g_curPitch, g_moving ? "MOV" : "   ");
    float ly = 0, lp = 0;
    bool live = g_model.liveTarget(millis(), &ly, &lp);
    d.setCursor(4, 116);
    if (live) d.printf("live  y%4.0f p%3.0f      ", ly, lp);
    else      d.printf("live  stale  mem y%4.0f", e.yawDeg);
    d.fillRect(0, 160, 320, 24, TFT_BLACK);
    d.setCursor(4, 162);
    d.printf("mem   y%4.0f p%3.0f c%3u", e.yawDeg, e.pitchDeg, e.conf);
    d.setCursor(4, 140);
    d.printf("follow %s", g_follow ? "ON " : "OFF");
}

void setup() {
    Serial.begin(115200);
    M5StackChan.begin();                  // M5.begin + PY32 (servo power on) + servos
    M5StackChan.setServoPowerEnabled(true);
    M5.Display.setRotation(1);
    M5.Display.fillScreen(TFT_BLACK);
    delay(100);

    restoreModel();

    if (!look::begin()) {
        M5.Display.setTextSize(2);
        M5.Display.drawString("camera init FAIL", 10, 200);
    }
    g_lastPeriodicMs = millis();
    Serial.println("[look] ready. f=follow r=reset p=print s=save o=find t=tune b=byteswap c/C=dump w=awb");
}

void loop() {
    M5StackChan.update();
    handleSerial();

    uint32_t now = millis();
    static uint32_t lastServoPoll = 0;
    if (now - lastServoPoll >= 100) {
        lastServoPoll = now;
        g_moving = M5StackChan.Motion.isMoving();
        g_curYaw = M5StackChan.Motion.getCurrentXAngle() / 10.0f;
        g_curPitch = M5StackChan.Motion.getCurrentYAngle() / 10.0f;
        look::setMoving(g_moving);
        g_model.noteMoving(g_moving);
    }

    if (look::poll(&g_sample)) {
        bool has = look::bestTarget(g_sample, &g_tBearing, &g_tElev, &g_tConf, &g_tSource);
        if (has && !g_sample.quarantined) {
            float absYaw = g_curYaw + kYawSign * (g_tBearing / 100.0f) * (kCameraHfovDeg * 0.5f);
            float absPitch = g_curPitch + kElevSign * (g_tElev / 100.0f) * (kCameraVfovDeg * 0.5f);
            absYaw = clampf(absYaw, -kYawLimitDeg, kYawLimitDeg);
            absPitch = clampf(absPitch, kPitchMinDeg, kPitchMaxDeg);
            g_model.observe(absYaw, absPitch, (uint8_t)g_tConf, now);
        }
        if (!has) g_tConf = 0;
    }
    g_model.decay(now);

    float ty = 0, tp = 0;
    bool doMove = false;
    if (g_model.wantLiveMove(g_curYaw, g_curPitch, now, &ty, &tp)) {
        doMove = true;
        Serial.printf("[live] yaw=%.0f pitch=%.0f conf=%u src=%c (head %.0f/%.0f) follow=%d\n",
                      ty, tp, g_model.liveConf(), g_tSource ? g_tSource : '-', g_curYaw, g_curPitch, g_follow ? 1 : 0);
    } else if (g_model.wantFallback(g_curYaw, g_curPitch, now, &ty, &tp)) {
        doMove = true;
        Serial.printf("[memory] fallback yaw=%.0f pitch=%.0f (head %.0f/%.0f) follow=%d\n",
                      ty, tp, g_curYaw, g_curPitch, g_follow ? 1 : 0);
    }
    if (doMove) {
        ty = clampf(ty, -kYawLimitDeg, kYawLimitDeg);
        tp = clampf(tp, kPitchMinDeg, kPitchMaxDeg);
        if (g_follow) {
            M5StackChan.Motion.move((int)(ty * 10.0f), (int)(tp * 10.0f), kMoveSpeed);
            look::setMoving(true);
            g_model.noteMoving(true);
            g_moving = true;
        }
    }

    // persistence: periodic + conf crossing 60, rate-limited to one write / 60 s
    bool crossed = g_lastConf < kSaveConfCross && g_tConf >= kSaveConfCross;
    g_lastConf = g_tConf;
    bool periodic = now - g_lastPeriodicMs >= kPeriodicSaveMs;
    if ((periodic || crossed) && (g_lastSaveMs == 0 || now - g_lastSaveMs >= kMinSaveGapMs)) {
        g_lastPeriodicMs = now;
        saveModel(periodic ? "periodic" : "conf>=60");
    }

    static uint32_t lastLog = 0;
    if (now - lastLog >= 1000) {
        lastLog = now;
        Serial.printf("[look] face=%d,%d conf=%d | motion=%d,%d conf=%d -> %c fps=%d cost=%luus\n",
                      g_sample.face_bearing, g_sample.face_elev, g_sample.face_conf,
                      g_sample.bearing, g_sample.elev, g_sample.conf,
                      g_tSource ? g_tSource : '-', look::fps(), (unsigned long)g_sample.costUs);
    }
    static uint32_t lastDraw = 0;
    if (now - lastDraw >= 200) {
        lastDraw = now;
        drawOverlay();
    }
    delay(5);
}
