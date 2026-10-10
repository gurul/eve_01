// look.cpp — camera grab, motion centroid, skin-blob face, fusion. Core 0.
//
// Camera config is a copy of M5CoreS3 1.0.1 src/utility/GC0308.cpp (the
// working reference for this sensor on this core), with the frame settings
// changed to CAMERA_GRAB_LATEST. I2C sharing follows that file too:
// M5.In_I2C.release() before esp_camera_init, SCCB on pins 12/11 with
// sccb_i2c_port = -1. M5Unified re-binds the pins lazily on its next transfer.
//
// Per frame: one pass over the 320x240 RGB565 buffer builds an 80x60 RGB cell
// grid (4x4 mean); each cell is classified as skin (mask) and folded into a
// 40x30 gray grid for the motion layer. Then a flood-fill labelling over the
// 4800-cell mask picks the face blob.
#include "look.h"

#include <M5Unified.h>
#include <esp_camera.h>
#include <freertos/FreeRTOS.h>
#include <freertos/semphr.h>
#include <freertos/task.h>
#include <esp_heap_caps.h>
#include <string.h>
#include <img_converters.h>     // fmt2jpg (esp32-camera)
#include <mbedtls/base64.h>

namespace look {
namespace {

camera_config_t camera_config = {
    .pin_pwdn     = -1,
    .pin_reset    = -1,
    .pin_xclk     = -1,  // external 20 MHz oscillator on the CoreS3 camera
    .pin_sccb_sda = 12,
    .pin_sccb_scl = 11,
    .pin_d7       = 47,
    .pin_d6       = 48,
    .pin_d5       = 16,
    .pin_d4       = 15,
    .pin_d3       = 42,
    .pin_d2       = 41,
    .pin_d1       = 40,
    .pin_d0       = 39,

    .pin_vsync = 46,
    .pin_href  = 38,
    .pin_pclk  = 45,

    .xclk_freq_hz = 20000000,
    .ledc_timer   = LEDC_TIMER_0,
    .ledc_channel = LEDC_CHANNEL_0,

    .pixel_format  = PIXFORMAT_RGB565,
    .frame_size    = FRAMESIZE_QVGA,
    .jpeg_quality  = 0,
    .fb_count      = 2,
    .fb_location   = CAMERA_FB_IN_PSRAM,
    .grab_mode     = CAMERA_GRAB_LATEST,
    .sccb_i2c_port = -1,
};

constexpr int kSkinCells = kSkinW * kSkinH;   // 4800
constexpr int kMapCells = kMapW * kMapH;      // 1200

uint8_t s_prev[kMapCells];
uint8_t s_cur[kMapCells];
uint16_t s_gray16[kMapCells];                 // sum of 4 skin cells' luma
uint16_t s_rowSum[kSkinW][3];                 // one block row of R,G,B sums
uint8_t s_skin[kSkinCells];                   // 1 = skin
uint16_t s_label[kSkinCells];                 // blob id, 0 = none
uint16_t s_stack[kSkinCells];                 // flood-fill stack
bool s_havePrev = false;

SemaphoreHandle_t s_lock = nullptr;
Sample s_latest = {};
Stats s_stats = {};
volatile bool s_moving = false;
volatile uint32_t s_lastMovingMs = 0;
volatile int s_fps = 0;
volatile bool s_swap = kRgb565ByteSwap;
int s_crMin = 255, s_crMax = 0;
uint32_t s_cSum[6];                           // centre R,G,B,Y,Cb,Cr sums
uint8_t s_cell[kSkinCells * 3];               // post-decode RGB888 per cell, last frame
uint8_t s_dump[kSkinCells * 3];               // snapshot for the dump task
volatile int s_dumpReq = 0;                   // 1 = current order, 2 = opposite
bool s_dumpSwap = false;                      // order the snapshot was decoded with
TaskHandle_t s_dumpTask = nullptr;
bool s_awb = true;

float s_bearingEma = 0.0f;
float s_elevEma = 0.0f;
float s_confEma = 0.0f;
float s_faceBearingEma = 0.0f;
float s_faceElevEma = 0.0f;
float s_faceConfEma = 0.0f;
uint32_t s_lastFaceMs = 0;

inline void toYCbCr(int r, int g, int b, int* y, int* cb, int* cr) {
    *y  = (77 * r + 150 * g + 29 * b) >> 8;
    *cb = 128 + ((-43 * r - 85 * g + 128 * b) >> 8);
    *cr = 128 + ((128 * r - 107 * g - 21 * b) >> 8);
}

inline bool isSkin(int r, int g, int b, int y, int cb, int cr) {
    // RGB rule (Kovac): R>95, G>40, B>20, R>G, R>B, |R-G|>15
    if (r <= kSkinRMin || g <= kSkinGMin || b <= kSkinBMin) return false;
    if (r <= g || r <= b) return false;
    int d = r - g; if (d < 0) d = -d;
    if (d <= kSkinRGDiffMin) return false;
    // YCbCr (Chai-Ngan): 77<=Cb<=127, 133<=Cr<=173, Y>=40
    if (y < kSkinYMin) return false;
    if (cb < kSkinCbMin || cb > kSkinCbMax) return false;
    if (cr < kSkinCrMin || cr > kSkinCrMax) return false;
    return true;
}

// One pass over the frame: 80x60 RGB cells -> skin mask + 40x30 gray.
void downscale(const uint8_t* buf, bool swap) {
    for (int i = 0; i < kMapCells; ++i) s_gray16[i] = 0;
    for (int i = 0; i < 6; ++i) s_cSum[i] = 0;
    s_crMin = 255; s_crMax = 0;
    for (int cy = 0; cy < kSkinH; ++cy) {
        for (int cx = 0; cx < kSkinW; ++cx) { s_rowSum[cx][0] = 0; s_rowSum[cx][1] = 0; s_rowSum[cx][2] = 0; }
        for (int yy = 0; yy < kSkinBlock; ++yy) {
            const uint8_t* row = buf + ((size_t)(cy * kSkinBlock + yy) * kFrameW) * 2;
            for (int x = 0; x < kFrameW; ++x) {
                // RGB565 pixel: default little-endian uint16 (byte0 low), 'b' flips.
                uint16_t px = swap ? (uint16_t)((row[x * 2] << 8) | row[x * 2 + 1])
                                   : (uint16_t)(row[x * 2] | (row[x * 2 + 1] << 8));
                uint16_t* c = s_rowSum[x >> 2];
                c[0] += (px >> 8) & 0xF8;        // R8 (5 msb)
                c[1] += (px >> 3) & 0xFC;        // G8 (6 msb)
                c[2] += (px << 3) & 0xF8;        // B8 (5 msb)
            }
        }
        uint8_t* skinRow = s_skin + cy * kSkinW;
        uint16_t* grayRow = s_gray16 + (cy >> 1) * kMapW;
        for (int cx = 0; cx < kSkinW; ++cx) {
            int r = s_rowSum[cx][0] >> 4;   // /16 px per cell
            int g = s_rowSum[cx][1] >> 4;
            int b = s_rowSum[cx][2] >> 4;
            uint8_t* cell = s_cell + (cy * kSkinW + cx) * 3;
            cell[0] = (uint8_t)r; cell[1] = (uint8_t)g; cell[2] = (uint8_t)b;
            int y, cb, cr;
            toYCbCr(r, g, b, &y, &cb, &cr);
            skinRow[cx] = isSkin(r, g, b, y, cb, cr) ? 1 : 0;
            grayRow[cx >> 1] += (uint16_t)y;
            if (cr < s_crMin) s_crMin = cr;
            if (cr > s_crMax) s_crMax = cr;
            if (cx >= kSkinW / 2 - 4 && cx < kSkinW / 2 + 4 && cy >= kSkinH / 2 - 4 && cy < kSkinH / 2 + 4) {
                s_cSum[0] += r; s_cSum[1] += g; s_cSum[2] += b;
                s_cSum[3] += y; s_cSum[4] += cb; s_cSum[5] += cr;
            }
        }
    }
    for (int i = 0; i < kMapCells; ++i) s_cur[i] = (uint8_t)(s_gray16[i] >> 2);
}

struct Blob { int area, x0, y0, x1, y1; uint32_t xs, ys; };

// 4-connected flood fill over the skin mask. Fills stats, returns the picked blob (area 0 = none).
Blob findFace(Stats* st) {
    for (int i = 0; i < kSkinCells; ++i) s_label[i] = 0;
    Blob best = {0, 0, 0, 0, 0, 0, 0};
    int skinCells = 0, blobs = 0, largestArea = 0, largestW = 0, largestH = 0;
    uint16_t id = 0;
    for (int start = 0; start < kSkinCells; ++start) {
        if (!s_skin[start]) continue;
        ++skinCells;
        if (s_label[start]) continue;
        ++id; ++blobs;
        Blob b = {0, kSkinW, kSkinH, -1, -1, 0, 0};
        int sp = 0;
        s_stack[sp++] = (uint16_t)start;
        s_label[start] = id;
        while (sp > 0) {
            int i = s_stack[--sp];
            int x = i % kSkinW, y = i / kSkinW;
            b.area++; b.xs += x; b.ys += y;
            if (x < b.x0) b.x0 = x; if (x > b.x1) b.x1 = x;
            if (y < b.y0) b.y0 = y; if (y > b.y1) b.y1 = y;
            int n;
            if (x > 0)         { n = i - 1;     if (s_skin[n] && !s_label[n]) { s_label[n] = id; s_stack[sp++] = n; } }
            if (x < kSkinW - 1){ n = i + 1;     if (s_skin[n] && !s_label[n]) { s_label[n] = id; s_stack[sp++] = n; } }
            if (y > 0)         { n = i - kSkinW; if (s_skin[n] && !s_label[n]) { s_label[n] = id; s_stack[sp++] = n; } }
            if (y < kSkinH - 1){ n = i + kSkinW; if (s_skin[n] && !s_label[n]) { s_label[n] = id; s_stack[sp++] = n; } }
        }
        int w = b.x1 - b.x0 + 1, h = b.y1 - b.y0 + 1;
        if (b.area > largestArea) { largestArea = b.area; largestW = w; largestH = h; }
        if (w < kBlobMinSide || w > kBlobMaxSide || h < kBlobMinSide || h > kBlobMaxSide) continue;
        float aspect = (float)w / (float)h;
        if (aspect < kBlobAspectMin || aspect > kBlobAspectMax) continue;
        if (b.area > best.area) best = b;
    }
    st->skinCellsPerMille = skinCells * 1000 / kSkinCells;
    st->blobCount = blobs;
    st->largestArea = largestArea;
    st->largestW = largestW;
    st->largestH = largestH;
    st->pickedArea = best.area;
    return best;
}

void analyse(uint32_t nowMs, bool quarantined, uint32_t costUs) {
    bool motion = false;
    float bearing = s_bearingEma, elev = s_elevEma, conf = 0.0f;
    Stats st = {};
    Blob face = {0, 0, 0, 0, 0, 0, 0};

    if (s_havePrev && !quarantined) {
        uint32_t wsum = 0, xsum = 0, ysum = 0;
        int cells = 0;
        for (int y = 0; y < kMapH; ++y) {
            for (int x = 0; x < kMapW; ++x) {
                int i = y * kMapW + x;
                int d = (int)s_cur[i] - (int)s_prev[i];
                if (d < 0) d = -d;
                if (d < kDiffThreshold) continue;
                wsum += (uint32_t)d;
                xsum += (uint32_t)d * x;
                ysum += (uint32_t)d * y;
                ++cells;
            }
        }
        if (cells >= kMinMovingCells) {
            motion = true;
            float cx = (float)xsum / (float)wsum;
            float cy = (float)ysum / (float)wsum;
            bearing = (cx - (kMapW - 1) * 0.5f) / ((kMapW - 1) * 0.5f) * 100.0f;
            elev = -(cy - (kMapH - 1) * 0.5f) / ((kMapH - 1) * 0.5f) * 100.0f;
            float frac = (float)cells / (float)(kMapW * kMapH);
            conf = frac * kConfGain;
            if (conf > 100.0f) conf = 100.0f;
        }
    }
    if (!quarantined) face = findFace(&st);

    if (motion) {
        s_bearingEma += (bearing - s_bearingEma) * 0.4f;
        s_elevEma += (elev - s_elevEma) * 0.4f;
    }
    s_confEma += (conf - s_confEma) * 0.3f;

    float faceConf = 0.0f;
    if (face.area > 0) {
        float fx = (float)face.xs / (float)face.area;
        float fy = (float)face.ys / (float)face.area;
        float fb = (fx - (kSkinW - 1) * 0.5f) / ((kSkinW - 1) * 0.5f) * 100.0f;
        float fe = -(fy - (kSkinH - 1) * 0.5f) / ((kSkinH - 1) * 0.5f) * 100.0f;
        s_faceBearingEma += (fb - s_faceBearingEma) * kFaceConfAlpha;
        s_faceElevEma += (fe - s_faceElevEma) * kFaceConfAlpha;
        if (face.area <= kFaceAreaLo) faceConf = (float)face.area * kFaceConfLo / kFaceAreaLo;
        else faceConf = kFaceConfLo + (float)(face.area - kFaceAreaLo) * (kFaceConfHi - kFaceConfLo) / (kFaceAreaHi - kFaceAreaLo);
        if (faceConf > 100.0f) faceConf = 100.0f;
        s_lastFaceMs = nowMs;
    }
    s_faceConfEma += (faceConf - s_faceConfEma) * kFaceConfAlpha;
    if (face.area == 0 && (nowMs - s_lastFaceMs) >= kFaceHoldMs) s_faceConfEma = 0.0f;

    for (int i = 0; i < kMapCells; ++i) s_prev[i] = s_cur[i];
    s_havePrev = true;
    st.costUs = costUs;
    st.cR = s_cSum[0] / 64; st.cG = s_cSum[1] / 64; st.cB = s_cSum[2] / 64;
    st.cY = s_cSum[3] / 64; st.cCb = s_cSum[4] / 64; st.cCr = s_cSum[5] / 64;
    st.crMin = s_crMin; st.crMax = s_crMax;
    st.byteSwap = s_swap;

    xSemaphoreTake(s_lock, portMAX_DELAY);
    s_latest.bearing = (int)s_bearingEma;
    s_latest.elev = (int)s_elevEma;
    s_latest.conf = (int)(s_confEma + 0.5f);
    s_latest.face_bearing = (int)s_faceBearingEma;
    s_latest.face_elev = (int)s_faceElevEma;
    s_latest.face_conf = (int)(s_faceConfEma + 0.5f);
    s_latest.face_seen = face.area > 0;
    if (face.area > 0) {
        s_latest.face_x0 = (uint8_t)face.x0; s_latest.face_y0 = (uint8_t)face.y0;
        s_latest.face_x1 = (uint8_t)face.x1; s_latest.face_y1 = (uint8_t)face.y1;
    }
    s_latest.frameMs = nowMs;
    s_latest.seq++;
    s_latest.costUs = costUs;
    s_latest.quarantined = quarantined;
    s_stats = st;
    xSemaphoreGive(s_lock);
}

// ---- host frame stream (runs inside task(), core 0) ----
// Byte order: the sensor delivers RGB565 big-endian per pixel (bench frame
// dumps, kRgb565ByteSwap = true) and esp32-camera's JPEG encoder reads
// RGB565 input the same way (byte0 = RRRRRGGG), so the subsample copies the
// 16-bit words untouched.
volatile bool s_streamOn = false;
volatile bool s_streamPaused = false;
volatile uint8_t s_streamFps = 5;
volatile uint16_t s_streamW = 160, s_streamH = 120;
volatile int s_headYaw = 0, s_headPitch = 45;
uint8_t* s_sub = nullptr;        // PSRAM: subsampled RGB565, kStreamMaxW*kStreamMaxH*2
char*    s_line = nullptr;       // PSRAM: NDJSON line, base64 payload
// 64 KB: a 160x120 q60 stream frame is ~4 KB of JPEG (~6 KB of line); a
// 320x240 q85 snapshot of a busy desk is ~25-35 KB of JPEG (~45 KB of line).
constexpr size_t kLineCap = 64 * 1024;
volatile bool s_snapReq = false;
uint32_t s_streamSeq = 0, s_lastFrameMs = 0;
uint32_t s_statFrames = 0, s_statBytes = 0, s_statEncodeUs = 0, s_statAt = 0;

// JPEG-encode an RGB565 image and write ONE frame line. Returns the bytes
// written, 0 when the encode or the base64 did not fit. Bumps the seq.
size_t writeFrameLine(const uint8_t* rgb565, uint16_t w, uint16_t h, uint8_t quality,
                      int yaw, int pitch, bool snap) {
    uint8_t* jpg = nullptr;
    size_t jpgLen = 0;
    // fmt2jpg takes a non-const source pointer; it only reads it.
    if (!fmt2jpg(const_cast<uint8_t*>(rgb565), (size_t)w * h * 2, w, h, PIXFORMAT_RGB565, quality,
                 &jpg, &jpgLen) || !jpg) {
        if (jpg) free(jpg);
        return 0;
    }
    // The line starts with '\n' so a half-written loop() line (println is
    // two writes) is terminated before our JSON, and ends with '\n'.
    int head = snprintf(s_line, kLineCap,
                        "\n{\"frame\":{\"seq\":%lu,\"w\":%u,\"h\":%u,\"fmt\":\"jpeg\",\"b64\":\"",
                        (unsigned long)s_streamSeq, (unsigned)w, (unsigned)h);
    size_t b64Len = 0;
    size_t cap = kLineCap - head - 64;
    int rc = mbedtls_base64_encode((unsigned char*)s_line + head, cap, &b64Len, jpg, jpgLen);
    free(jpg);
    if (rc != 0) {
        if (snap) Serial.printf("[cam] snap did not fit: %u B jpeg\n", (unsigned)jpgLen);
        return 0;
    }
    int tail = snprintf(s_line + head + b64Len, 64, "\",\"yaw\":%d,\"pitch\":%d%s}}\n",
                        yaw, pitch, snap ? ",\"snap\":true" : "");
    size_t total = head + b64Len + tail;
    s_streamSeq++;
    Serial.write((const uint8_t*)s_line, total);   // one write: HWCDC holds tx_lock for the whole buffer
    return total;
}

// One full-size photo for the host's diary, on request. Independent of the
// stream (works with the stream off) but respects the wire pause.
void snapFrame(const uint8_t* buf, uint32_t nowMs) {
    if (!s_snapReq || s_streamPaused || !s_line) return;
    s_snapReq = false;
    uint32_t e0 = micros();
    size_t total = writeFrameLine(buf, kFrameW, kFrameH, kSnapJpegQ, s_headYaw, s_headPitch, true);
    Serial.printf("[cam] snap %ux%u q=%u: %u B line, %lu ms\n", (unsigned)kFrameW, (unsigned)kFrameH,
                  (unsigned)kSnapJpegQ, (unsigned)total, (unsigned long)((micros() - e0) / 1000));
    (void)nowMs;
}

void streamFrame(const uint8_t* buf, uint32_t nowMs) {
    if (!s_streamOn || s_streamPaused || !s_sub || !s_line) return;
    uint32_t period = 1000 / (s_streamFps ? s_streamFps : 1);
    if (s_lastFrameMs && nowMs - s_lastFrameMs < period) return;
    uint16_t w = s_streamW, h = s_streamH;
    if (w > kStreamMaxW) w = kStreamMaxW;
    if (h > kStreamMaxH) h = kStreamMaxH;
    if (w < 16 || h < 16) return;
    int yaw = s_headYaw, pitch = s_headPitch;   // pose at capture
    uint32_t e0 = micros();
    // nearest-neighbour subsample of the QVGA frame
    int sx = kFrameW / w, sy = kFrameH / h;
    if (sx < 1) sx = 1;
    if (sy < 1) sy = 1;
    const uint16_t* src = (const uint16_t*)buf;
    uint16_t* dst = (uint16_t*)s_sub;
    for (int y = 0; y < h; ++y) {
        const uint16_t* row = src + (size_t)(y * sy) * kFrameW;
        for (int x = 0; x < w; ++x) dst[y * w + x] = row[x * sx];
    }
    size_t total = writeFrameLine(s_sub, w, h, kStreamJpegQ, yaw, pitch, false);
    if (!total) return;
    uint32_t encodeUs = micros() - e0;
    s_lastFrameMs = nowMs;
    s_statFrames++; s_statBytes += total; s_statEncodeUs += encodeUs;
    if (s_statAt == 0) s_statAt = nowMs;
    if (nowMs - s_statAt >= 30000) {
        Serial.printf("[cam] %lu frames, %lu KB, %lu ms/encode\n",
                      (unsigned long)s_statFrames, (unsigned long)(s_statBytes / 1024),
                      (unsigned long)(s_statFrames ? s_statEncodeUs / s_statFrames / 1000 : 0));
        s_statFrames = 0; s_statBytes = 0; s_statEncodeUs = 0; s_statAt = nowMs;
    }
}

void task(void*) {
    uint32_t fpsWindowStart = millis();
    int frames = 0;
    for (;;) {
        uint32_t t0 = millis();
        camera_fb_t* fb = esp_camera_fb_get();
        if (!fb) {
            vTaskDelay(pdMS_TO_TICKS(20));
            continue;
        }
        if (fb->format == PIXFORMAT_RGB565 && fb->width == kFrameW && fb->height == kFrameH) {
            streamFrame(fb->buf, millis());   // host stream first: the frame is still whole
            snapFrame(fb->buf, millis());     // a requested photo, same whole frame
            uint32_t c0 = micros();
            const bool swap = s_swap;
            downscale(fb->buf, swap);
            int req = s_dumpReq;
            if (req) {
                s_dumpReq = 0;
                bool dumpSwap = (req == 2) ? !swap : swap;
                if (req == 2) downscale(fb->buf, dumpSwap); // s_skin/gray are refreshed below from the normal order
                xSemaphoreTake(s_lock, portMAX_DELAY);
                memcpy(s_dump, s_cell, sizeof(s_dump));
                s_dumpSwap = dumpSwap;
                xSemaphoreGive(s_lock);
                if (req == 2) downscale(fb->buf, swap);
                if (s_dumpTask) xTaskNotifyGive(s_dumpTask);
            }
            esp_camera_fb_return(fb);
            uint32_t now = millis();
            bool quarantined = s_moving || (now - s_lastMovingMs) < kQuietAfterMoveMs;
            analyse(now, quarantined, micros() - c0);
            ++frames;
        } else {
            esp_camera_fb_return(fb);
        }
        uint32_t now = millis();
        if (now - fpsWindowStart >= 1000) {
            s_fps = frames;
            frames = 0;
            fpsWindowStart = now;
        }
        uint32_t spent = now - t0;
        if (spent < kFramePeriodMs) vTaskDelay(pdMS_TO_TICKS(kFramePeriodMs - spent));
        else taskYIELD();
    }
}

void dumpTask(void*) {
    static char line[kSkinW * 6 + 2];
    static const char hex[] = "0123456789ABCDEF";
    for (;;) {
        ulTaskNotifyTake(pdTRUE, portMAX_DELAY);
        uint8_t* snap = (uint8_t*)heap_caps_malloc(sizeof(s_dump), MALLOC_CAP_8BIT);
        bool swap;
        xSemaphoreTake(s_lock, portMAX_DELAY);
        if (snap) memcpy(snap, s_dump, sizeof(s_dump));
        swap = s_dumpSwap;
        xSemaphoreGive(s_lock);
        if (!snap) { Serial.println("[frame] alloc failed"); continue; }
        Serial.printf("[frame] %d %d swap=%d\n", kSkinW, kSkinH, (int)swap);
        for (int y = 0; y < kSkinH; ++y) {
            const uint8_t* row = snap + y * kSkinW * 3;
            int o = 0;
            for (int x = 0; x < kSkinW * 3; ++x) {
                line[o++] = hex[row[x] >> 4];
                line[o++] = hex[row[x] & 0x0F];
            }
            line[o++] = '\n';
            Serial.write((const uint8_t*)line, o);
        }
        Serial.println("[frame] end");
        free(snap);
    }
}

} // namespace

bool begin() {
    if (!s_lock) s_lock = xSemaphoreCreateMutex();
    M5.In_I2C.release();  // same hand-off as M5CoreS3 GC0308::begin()
    esp_err_t err = esp_camera_init(&camera_config);
    if (err != ESP_OK) {
        Serial.printf("[look] esp_camera_init failed: 0x%x\n", (unsigned)err);
        return false;
    }
    sensor_t* s = esp_camera_sensor_get();
    Serial.printf("[look] camera up, sensor pid=0x%x\n", s ? s->id.PID : 0);
    if (s) {
        Serial.printf("[look] sensor defaults: awb=%u aec=%u agc=%u\n",
                      s->status.awb, s->status.aec, s->status.agc);
        int ra = s->set_whitebal ? s->set_whitebal(s, 1) : -1;
        int re = s->set_exposure_ctrl ? s->set_exposure_ctrl(s, 1) : -1;
        int rg = s->set_gain_ctrl ? s->set_gain_ctrl(s, 1) : -1;
        Serial.printf("[look] enable awb/aec/agc -> %d/%d/%d, now awb=%u aec=%u agc=%u byteswap=%d\n",
                      ra, re, rg, s->status.awb, s->status.aec, s->status.agc, (int)s_swap);
    }
    // Host stream buffers (PSRAM): subsampled frame + one NDJSON line.
    s_sub  = (uint8_t*)heap_caps_malloc((size_t)kStreamMaxW * kStreamMaxH * 2, MALLOC_CAP_SPIRAM | MALLOC_CAP_8BIT);
    s_line = (char*)heap_caps_malloc(kLineCap, MALLOC_CAP_SPIRAM | MALLOC_CAP_8BIT);
    if (!s_sub || !s_line) Serial.println("[cam] stream buffers FAILED (no PSRAM?)");
    xTaskCreatePinnedToCore(task, "look", 12288, nullptr, 2, nullptr, 0);   // +4 KB for fmt2jpg
    xTaskCreatePinnedToCore(dumpTask, "lookdump", 4096, nullptr, 1, &s_dumpTask, 1);
    return true;
}

void setStream(bool on, uint8_t fps, uint16_t w, uint16_t h) {
    if (fps < 1) fps = 1;
    if (fps > 15) fps = 15;
    s_streamFps = fps;
    if (w) s_streamW = w > kStreamMaxW ? kStreamMaxW : w;
    if (h) s_streamH = h > kStreamMaxH ? kStreamMaxH : h;
    if (on != s_streamOn) {
        s_streamOn = on;
        s_lastFrameMs = 0;
        Serial.printf("[cam] stream %s fps=%u %ux%u q=%u\n", on ? "on" : "off",
                      (unsigned)s_streamFps, (unsigned)s_streamW, (unsigned)s_streamH, (unsigned)kStreamJpegQ);
    }
}
void setStreamPaused(bool paused) { s_streamPaused = paused; }
void requestSnap() { s_snapReq = true; }
void setHeadPose(int yawDeg, int pitchDeg) { s_headYaw = yawDeg; s_headPitch = pitchDeg; }

void requestDump(bool oppositeOrder) { s_dumpReq = oppositeOrder ? 2 : 1; }

void printSensor() {
    sensor_t* s = esp_camera_sensor_get();
    if (!s) { Serial.println("[look] sensor: none"); return; }
    Serial.printf("[look] sensor pid=0x%x awb=%u aec=%u agc=%u awb_gain=%u wb_mode=%u byteswap=%d\n",
                  s->id.PID, s->status.awb, s->status.aec, s->status.agc,
                  s->status.awb_gain, s->status.wb_mode, (int)s_swap);
}

bool toggleAwb() {
    sensor_t* s = esp_camera_sensor_get();
    s_awb = !s_awb;
    int r = (s && s->set_whitebal) ? s->set_whitebal(s, s_awb ? 1 : 0) : -1;
    Serial.printf("[look] set_whitebal(%d) -> %d, status awb=%u\n", (int)s_awb, r, s ? s->status.awb : 0);
    return s_awb;
}

void setMoving(bool moving) {
    s_moving = moving;
    if (moving) s_lastMovingMs = millis();
}

bool poll(Sample* out) {
    static uint32_t lastSeq = 0;
    xSemaphoreTake(s_lock, portMAX_DELAY);
    *out = s_latest;
    xSemaphoreGive(s_lock);
    bool fresh = out->seq != lastSeq;
    lastSeq = out->seq;
    return fresh;
}

void stats(Stats* out) {
    xSemaphoreTake(s_lock, portMAX_DELAY);
    *out = s_stats;
    xSemaphoreGive(s_lock);
}

int fps() { return s_fps; }

void setByteSwap(bool swap) { s_swap = swap; }
bool byteSwap() { return s_swap; }

bool bestTarget(const Sample& s, int* bearing, int* elev, int* conf, char* source) {
    if (s.face_conf >= kFuseFaceMinConf) {
        *bearing = s.face_bearing; *elev = s.face_elev; *conf = s.face_conf; *source = 'F';
        return true;
    }
    if (s.conf >= kFuseMotionMinConf) {
        *bearing = s.bearing; *elev = s.elev; *conf = s.conf; *source = 'M';
        return true;
    }
    *source = 0;
    return false;
}

} // namespace look
