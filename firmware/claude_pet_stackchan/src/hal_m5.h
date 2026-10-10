#pragma once
// CoreS3 core peripherals behind plain functions.
//
// Why this file exists: M5Unified declares a global object named `M5`, and
// the pet code (data.h, xfer.h, main.cpp) also names its board object `M5`
// (an M5Compat, see board_compat.h). data.h must stay byte-identical with
// the Freenove build, so the pet's `M5` keeps its name. The two globals never
// meet: only hal_m5.cpp and body.cpp include <M5Unified.hpp>/<M5StackChan.h>,
// and neither of those includes board_compat.h. Everything else talks to the
// CoreS3 through the functions below. No macro renames anything.
#include <stdint.h>

namespace m5gfx { class M5GFX; }
using M5GFX = m5gfx::M5GFX;

// Serial buffers + Serial.begin + M5StackChan.begin() (which calls M5.begin()
// once). Forces the display to landscape. Call once from setup().
void   halBegin();
// M5.update(): screen touch and buttons; top touch is unpowered in quiet mode.
void   halUpdate();
// Temporary bench isolation, restored after 45 seconds or target="restore".
bool   halNoiseTest(const char* target);
void   halNoiseTestUpdate();
bool   halNoiseMotorsOff();
// External 5 V on/off (not persisted) and a fresh servo read in 0.1 deg; true when the readback matches.
bool   halExtPower(bool on, int* yaw, int* pitch);
// The CoreS3 panel (M5.Display). Valid to take the reference before begin().
M5GFX& halDisplay();

// Touch panel, display coordinates (landscape 320x240). False when no finger.
bool   halTouch(int* x, int* y);

// Display backlight 0..255 (AXP2101 DLDO1 through M5GFX).
void   halBrightness(uint8_t level);

// Battery / bus (AXP2101 via M5.Power). Millivolts and milliamps.
int    halBatteryMv();
int    halBatteryMa();
int    halVbusMv();
// AXP power key short click since the last update.
bool   halPowerClicked();
// Backlight off + deep sleep, wake on touch.
void   halDeepSleep();
// Cut all power through the AXP2101 (not a sleep): the board is off until the power key or USB power.
void   halPowerOff();

// BM8563 RTC. Falls back to the system clock when the RTC is absent.
void   halRtcGet(int* h, int* m, int* s, int* wday, int* mon, int* mday, int* year);
void   halRtcSet(int h, int m, int s, int wday, int mon, int mday, int year);
