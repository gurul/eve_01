// look.h — perception for the StackChan camera (GC0308): motion centroid +
// skin-colour face blob, fused into one target. A FreeRTOS task on core 0
// grabs frames and analyses them. loop() polls the latest sample without
// blocking.
#pragma once

#include <stdint.h>

namespace look {

struct Sample {
    // motion layer
    int bearing;      // -100 (left edge) .. 100 (right edge) of the frame
    int elev;         // -100 (bottom) .. 100 (top)
    int conf;         // 0..100, EMA of the moving-pixel fraction (clipped)
    // face (skin blob) layer
    int face_bearing;
    int face_elev;
    int face_conf;    // 0..100 from blob area, EMA, 0 within 1 s of no blob
    uint8_t face_x0, face_y0, face_x1, face_y1; // blob box in skin-grid cells (inclusive)
    bool face_seen;   // a valid blob was found in this frame
    // bookkeeping
    uint32_t frameMs; // millis() when the frame was analysed
    uint32_t seq;     // increments per analysed frame
    uint32_t costUs;  // downscale + analysis time for this frame
    bool quarantined; // frame was dropped because the head was moving
};

// Bench stats for retuning the skin bounds (serial 't').
struct Stats {
    int skinCellsPerMille; // skin cells / all cells * 1000, last frame
    int blobCount;         // 4-connected blobs in the last frame
    int largestArea;       // cells, any blob (before the size/aspect filter)
    int largestW, largestH;
    int pickedArea;        // cells, the blob that passed the filter (0 = none)
    uint32_t costUs;
    // decode proof: mean of the 8x8 centre cells (cx 36..43, cy 26..33) and
    // the Cr range over the whole grid. A face at centre: R > G > B, Cr 130..160.
    int cR, cG, cB;
    int cY, cCb, cCr;
    int crMin, crMax;
    bool byteSwap;         // decode mode in force for this frame
};

// Camera up + task started. Returns false when esp_camera_init fails.
bool begin();

// Call every loop with Motion.isMoving(). Frames captured while moving, or
// within kQuietAfterMoveMs after the flag drops, are not used for detection.
void setMoving(bool moving);

// Copies the latest sample. Returns true when seq changed since the last poll.
bool poll(Sample* out);

// Copies the last-frame stats.
void stats(Stats* out);

// RGB565 byte order. false (default, kRgb565ByteSwap) = little-endian uint16
// per pixel: byte0 = GGGBBBBB, byte1 = RRRRRGGG, which is what M5CoreS3's
// camera example implies when it pushes fb->buf as rgb565_t. true = byte0 is
// the high byte (RRRRRGGG first). Serial 'b' flips it live for the bench.
void setByteSwap(bool swap);
bool byteSwap();

// Bench evidence. requestDump(false) dumps the 80x60 RGB888 cell grid from the
// classifier's own decode path; requestDump(true) re-decodes the next frame
// with the opposite byte order. A low-priority task prints:
//   [frame] 80 60 swap=<0|1>
//   60 lines of 80 RRGGBB hex triplets
//   [frame] end
void requestDump(bool oppositeOrder);

// Sensor controls for the bench. printSensor() prints awb/aec/agc + byteswap.
void printSensor();
bool toggleAwb();   // returns the new AWB state

// Analysed frames per second over the last second.
int fps();

// ---- host frame stream -----------------------------------------------------
// {"cmd":"cam","on":true,"fps":5,"w":160,"h":120}: the look task subsamples
// each QVGA frame to w x h (2x2 nearest; sensor stays QVGA so the on-board
// detector keeps its grid), JPEG-encodes it (fmt2jpg, quality kStreamJpegQ)
// and writes ONE NDJSON line from the task, never from loop():
//   {"frame":{"seq":n,"w":160,"h":120,"fmt":"jpeg","b64":"...","yaw":Y,"pitch":P}}
// Frames are capped at fps, skipped while the previous line is still being
// written or while setStreamPaused(true) (a transfer owns the wire).
// Every 30 s the task prints `[cam] %d frames, %d KB, %d ms/encode`.
void setStream(bool on, uint8_t fps, uint16_t w, uint16_t h);
void setStreamPaused(bool paused);
// {"cmd":"snap"}: the next whole frame goes out once at full sensor size
// (320x240, quality kSnapJpegQ) as a frame line with "snap":true, on the same
// seq counter, regardless of the stream state. The host keeps it as a photo.
void requestSnap();
// Head pose echoed into each frame line (the pose the servos were streamed
// at capture time, so host latency cannot corrupt the absolute angles).
void setHeadPose(int yawDeg, int pitchDeg);
constexpr uint8_t kStreamJpegQ = 60;
constexpr uint8_t kSnapJpegQ = 85;
constexpr uint16_t kStreamMaxW = 160, kStreamMaxH = 120;

// Fusion: face when face_conf >= kFuseFaceMinConf (a still face keeps the
// gaze), else motion when conf >= kFuseMotionMinConf, else none.
// source = 'F', 'M', or 0. Returns true when a target exists.
bool bestTarget(const Sample& s, int* bearing, int* elev, int* conf, char* source);

// ---- decode ----------------------------------------------------------------
constexpr bool kRgb565ByteSwap = true;   // bench 2026-09-05: frame dumps prove big-endian per pixel (swap=1 is the natural image)

// ---- frame geometry --------------------------------------------------------
constexpr int kFrameW = 320, kFrameH = 240;   // QVGA RGB565 from the sensor
constexpr int kSkinBlock = 4;                 // skin grid: 4x4 px cells
constexpr int kSkinW = kFrameW / kSkinBlock;  // 80
constexpr int kSkinH = kFrameH / kSkinBlock;  // 60
constexpr int kBlock = 8;                     // motion grid: 8x8 px cells (2x2 skin cells)
constexpr int kMapW = kFrameW / kBlock;       // 40
constexpr int kMapH = kFrameH / kBlock;       // 30

// ---- motion tunables -------------------------------------------------------
constexpr int kDiffThreshold = 18;            // gray delta that counts as motion
constexpr int kConfGain = 400;                // conf = fraction * gain (25% moving = 100)
constexpr int kMinMovingCells = 3;            // below this the frame is "still"

// ---- skin classifier (Chai-Ngan YCbCr bounds AND the RGB rule) -------------
constexpr int kSkinCbMin = 77, kSkinCbMax = 127;
constexpr int kSkinCrMin = 133, kSkinCrMax = 173;
constexpr int kSkinYMin = 40;
constexpr int kSkinRMin = 95, kSkinGMin = 40, kSkinBMin = 20;
constexpr int kSkinRGDiffMin = 15;            // |R-G| > 15

// ---- blob filter -----------------------------------------------------------
constexpr int kBlobMinSide = 4, kBlobMaxSide = 40;   // bbox side in skin cells
constexpr float kBlobAspectMin = 0.5f, kBlobAspectMax = 2.0f; // w/h
constexpr int kFaceAreaLo = 16, kFaceConfLo = 30;    // 16 cells -> conf 30
constexpr int kFaceAreaHi = 100, kFaceConfHi = 100;  // 100 cells -> conf 100
constexpr float kFaceConfAlpha = 0.4f;
constexpr uint32_t kFaceHoldMs = 1000;        // no blob this long -> face_conf = 0

// ---- fusion ----------------------------------------------------------------
constexpr int kFuseFaceMinConf = 25;
constexpr int kFuseMotionMinConf = 20;

// ---- timing ----------------------------------------------------------------
constexpr uint32_t kFramePeriodMs = 100;      // ~10 fps analysis rate
constexpr uint32_t kQuietAfterMoveMs = 400;   // discard window after a move ends

} // namespace look
