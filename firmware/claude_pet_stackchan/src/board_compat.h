#pragma once
// M5StackChan K151 (CoreS3) board compat layer.
//
// Keeps the M5StickCPlus-shaped API surface the pet code compiles against
// (M5.Lcd, M5.BtnA/BtnB, M5.Rtc, M5.Axp, M5.Beep, M5.Imu, touch + gestures,
// ledSet) but implements it on M5Unified + M5GFX + StackChan-BSP.
//
// Naming: the pet code writes `M5.` (an M5Compat) because data.h must stay
// byte-identical with the Freenove build and it calls M5.Rtc.SetTime().
// M5Unified also has a global `::M5`. Two rules keep them apart, no macro:
//   1. The compat object is `pet::M5`, exported with `using pet::M5;`, so
//      its link symbol never collides with M5Unified's `::M5`.
//   2. The two never share a translation unit: this header includes only
//      <M5GFX.h> (no global objects), and the two TUs that include
//      <M5Unified.hpp>/<M5StackChan.h> — hal_m5.cpp and body.cpp — never
//      include this header. See hal_m5.h.
//
// Display types: TFT_eSPI is an alias for LovyanGFX (the M5GFX base), and
// TFT_eSprite derives from LGFX_Sprite to add the created() query TFT_eSPI
// had. M5.Lcd is a reference to M5.Display.
//
// Screen is landscape 320x240. Touch zones (see board_compat.cpp):
//   y >= STRIP_Y      → button strip: left half BtnA, right half BtnB
//   anywhere          → pet gestures (tap, hold, swipes, scrub)
// Body top-touch: front tap = pet tap, back tap = BtnB, middle hold = PTT.
#include <Arduino.h>
#include <M5GFX.h>
#include <FS.h>
#include <esp_mac.h>   // esp_read_mac / ESP_MAC_BT
// M5's display lib exposed bare color names; map to the TFT_ constants.
#ifndef GREEN
#define GREEN TFT_GREEN
#endif
#ifndef RED
#define RED TFT_RED
#endif

using TFT_eSPI = LovyanGFX;
class TFT_eSprite : public LGFX_Sprite {
 public:
  using LGFX_Sprite::LGFX_Sprite;
  bool created() const { return getBuffer() != nullptr; }
};

// ---- geometry ----
constexpr int SCREEN_W = 320;
constexpr int SCREEN_H = 240;
constexpr int STRIP_Y  = 204;   // button strip = transcript HUD rows

// ---- RTC structs (M5-compatible field names) ----
struct RTC_TimeTypeDef { uint8_t Hours, Minutes, Seconds; };
struct RTC_DateTypeDef { uint8_t WeekDay, Month, Date; uint16_t Year; };

class RtcCompat {
 public:
  void GetTime(RTC_TimeTypeDef* t);
  void GetDate(RTC_DateTypeDef* d);
  void SetTime(RTC_TimeTypeDef* t);
  void SetDate(RTC_DateTypeDef* d);
};

class ImuCompat {
 public:
  void Init() {}
  // Deliberately inert. The CoreS3 (and its BMI270) sits in the robot's
  // head: every pitch move changes the gravity vector, so the pet's
  // face-down nap, shake, and orientation logic would fire on the robot's
  // own choreography. Report a steady upright 1g instead.
  void getAccelData(float* ax, float* ay, float* az) { *ax = 0; *ay = 0; *az = 1.0f; }
};

class AxpCompat {
 public:
  void  begin();
  void  ScreenBreath(uint8_t pct);       // 0..100 → M5.Display.setBrightness
  void  SetLDO2(bool on);                // backlight hard on/off
  float GetBatVoltage();                 // volts (AXP2101)
  float GetBatCurrent();                 // mA, positive = charging (AXP2101)
  float GetVBusVoltage();                // volts
  uint8_t GetBtnPress();                 // 0x02 = power key short click
  float GetTempInAXP192();
  void  PowerOff();                      // backlight off + deep sleep
 private:
  uint8_t _pct = 80;
  bool    _on  = true;
  void    _apply();
};

class BeepCompat {
 public:
  void begin() {}
  void update() {}
  void tone(uint16_t freq, uint16_t ms);   // M5.Speaker
};

// M5-style button fed from touch zones each M5.update()
class TouchButton {
 public:
  void feed(bool down, uint32_t now);
  bool isPressed()  const { return _down; }
  bool wasPressed() const { return _down && !_prev; }
  bool wasReleased()const { return !_down && _prev; }
  bool pressedFor(uint32_t ms) const { return _down && (millis() - _downAt) >= ms; }
  // Withdraw the current press without generating a release edge. Used when a
  // touch that started in the button strip turns out to be a swipe: the
  // gesture consumes it, and the button must not also fire on lift.
  void cancel() { _down = false; _prev = false; }
 private:
  bool _down = false, _prev = false;
  uint32_t _downAt = 0;
};

class M5Compat {
 public:
  M5Compat();
  M5GFX&     Lcd;           // M5.Display
  RtcCompat  Rtc;
  ImuCompat  Imu;
  AxpCompat  Axp;
  BeepCompat Beep;
  TouchButton BtnA, BtnB;   // bottom-left / bottom-right touch zones (+ body back tap → BtnB)

  void begin();
  void update();            // polls panel touch + body touch, updates buttons + gestures

  // Raw touch state (landscape coords, 320x240)
  bool touching() const { return _touchDown; }
  int  touchX()   const { return _tx; }
  int  touchY()   const { return _ty; }

  // One-shot gesture events in the pet area (consumed on read)
  bool petTapped();     // quick tap on the pet (panel or body front zone)
  bool petTapWasBody() const { return _tapFromBody; }   // valid right after petTapped()
  bool petScrubbed();   // rapid left-right scrubbing → dizzy
  // Press-and-hold on the pet. Fires start after 600ms of a steady press
  // (<20px drift), end on release or on leaving the pet zone. A hold never
  // also fires tap (tap needs release <450ms) and suppresses scrub detection
  // while active. The body's middle touch zone is a second hold source with
  // the same timing and the same events.
  // Nothing acts on a hold any more — push-to-talk is gone. The edges exist
  // only to be drained in main.cpp so they cannot latch, and the latch itself
  // survives only to suppress tap/scrub during a steady press. There is
  // deliberately no level query: reading it is how a hold last became a
  // listening pose the host never asked for.
  bool petHoldStarted();
  bool petHoldEnded();
  // Downward swipe on the pet → Enter on the host. Fires mid-drag the moment
  // the threshold is crossed (not on release) so it feels immediate.
  bool petSwipedDown();
  // Horizontal swipes → option navigation on the host (next/prev in Claude
  // Code's pickers). Same mid-drag threshold model as swipe-down; the shared
  // one-gesture-per-press latch means a swipe is down OR left OR right.
  bool petSwipedLeft();
  bool petSwipedRight();

 private:
  bool _touchDown = false;
  int  _tx = 0, _ty = 0;
  // Release debounce: consecutive empty polls required before believing the
  // finger is gone. A single dropped read used to fire a spurious tap.
  static const uint8_t TOUCH_UP_POLLS = 3;
  uint8_t _upPolls = 0;
  // gesture tracking
  bool     _petTouch = false;
  uint32_t _petDownMs = 0;
  int      _petDownX = 0, _petDownY = 0;
  int      _lastX = 0;
  int8_t   _dir = 0;
  uint8_t  _reversals = 0;
  bool     _evTap = false, _evScrub = false;
  bool     _tapFromBody = false;
  bool     _holdActive = false;
  bool     _evHoldStart = false, _evHoldEnd = false;
  bool     _evSwipeDown = false;
  bool     _evSwipeLeft = false, _evSwipeRight = false;
  bool     _swipeFired = false;   // one gesture per press, not one per frame
  // body top-touch tracking
  bool     _frontDown = false;
  uint32_t _frontDownMs = 0;
  bool     _midDown = false;
  uint32_t _midDownMs = 0;
  bool     _bodyHoldActive = false;
  void updateBodyTouch(uint32_t now);
};

// The object itself is pet::M5 so its link symbol cannot collide with
// M5Unified's ::M5. The using-declaration lets the pet sources keep
// writing `M5.` unchanged — they never see M5Unified's global.
namespace pet { extern M5Compat M5; }
using pet::M5;

// All 12 body LEDs one colour (cached: unchanged colours cost nothing).
void ledSet(uint8_t r, uint8_t g, uint8_t b);
