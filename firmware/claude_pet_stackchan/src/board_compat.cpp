#include "board_compat.h"
#include "hal_m5.h"
#include "body.h"
#include "chirp.h"

namespace pet { M5Compat M5; }

M5Compat::M5Compat() : Lcd(halDisplay()) {}

// ---- body LEDs ----
// main.cpp calls this every loop iteration; skip unchanged colours so the
// PY32 expander only sees an I2C transaction when something changes.
void ledSet(uint8_t r, uint8_t g, uint8_t b) {
  static uint8_t lr = 0xFF, lg = 0xFF, lb = 0xFF;
  if (r == lr && g == lg && b == lb) return;
  bodySetLed(r, g, b);
  lr = r; lg = g; lb = b;
}

// ---- RTC (BM8563 via hal, system clock fallback) ----
static RTC_TimeTypeDef _pendT = {};
void RtcCompat::GetTime(RTC_TimeTypeDef* t) {
  int h, m, s, wd, mo, d, y;
  halRtcGet(&h, &m, &s, &wd, &mo, &d, &y);
  t->Hours = h; t->Minutes = m; t->Seconds = s;
}
void RtcCompat::GetDate(RTC_DateTypeDef* d) {
  int h, m, s, wd, mo, dd, y;
  halRtcGet(&h, &m, &s, &wd, &mo, &dd, &y);
  d->WeekDay = wd; d->Month = mo; d->Date = dd; d->Year = y;
}
void RtcCompat::SetTime(RTC_TimeTypeDef* t) { _pendT = *t; }
void RtcCompat::SetDate(RTC_DateTypeDef* d) {
  // SetTime is always called first by data.h; commit on SetDate.
  halRtcSet(_pendT.Hours, _pendT.Minutes, _pendT.Seconds,
            d->WeekDay, d->Month, d->Date, d->Year);
}

// ---- backlight / power / battery ----
void AxpCompat::begin() { _apply(); }
void AxpCompat::_apply() {
  halBrightness(_on ? (uint8_t)((uint32_t)_pct * 255 / 100) : 0);
}
void AxpCompat::ScreenBreath(uint8_t pct) { _pct = pct > 100 ? 100 : pct; _apply(); }
void AxpCompat::SetLDO2(bool on)          { _on = on; _apply(); }
float AxpCompat::GetBatVoltage()  { return halBatteryMv() / 1000.0f; }
float AxpCompat::GetBatCurrent()  { return (float)halBatteryMa(); }
float AxpCompat::GetVBusVoltage() { return halVbusMv() / 1000.0f; }
uint8_t AxpCompat::GetBtnPress()  { return halPowerClicked() ? 0x02 : 0; }
float AxpCompat::GetTempInAXP192() { return temperatureRead(); }
void AxpCompat::PowerOff() { halDeepSleep(); }

// UI beeps (card armed, swipe, scroll) go through the chirp synth so nothing
// relies on M5.Speaker.tone(); a beep is dropped while a phrase plays.
void BeepCompat::tone(uint16_t freq, uint16_t ms) { chirpBeep(freq, ms); }

// ---- touch buttons ----
void TouchButton::feed(bool down, uint32_t now) {
  _prev = _down;
  if (down && !_down) _downAt = now;
  _down = down;
}

void M5Compat::begin() {
  halBegin();
  Axp.begin();
  ledSet(0, 0, 0);
}

// Body top touch (Si12T, three zones). Front tap → pet tap. Back → BtnB
// (fed in update()). Middle held 600ms → push-to-talk hold, same events
// as the on-screen hold.
void M5Compat::updateBodyTouch(uint32_t now) {
  bool front = bodyTouchZone(0) > 0;
  bool mid   = bodyTouchZone(1) > 0;

  if (front && !_frontDown) { _frontDownMs = now; }
  else if (!front && _frontDown) {
    if (now - _frontDownMs < 450) { _evTap = true; _tapFromBody = true; }
  }
  _frontDown = front;

  if (mid && !_midDown) { _midDownMs = now; }
  if (mid && !_bodyHoldActive && now - _midDownMs >= 600) {
    _bodyHoldActive = true;
    _evHoldStart = true;
  }
  if (!mid && _bodyHoldActive) {
    _bodyHoldActive = false;
    _evHoldEnd = true;
  }
  _midDown = mid;
}

// Touch zone map (landscape 320x240):
//   y >= STRIP_Y → button strip: left half = BtnA, right half = BtnB
//   anywhere     → pet gestures (tap, hold, swipes, scrub)
void M5Compat::update() {
  halUpdate();
  uint32_t now = millis();
  int x, y;
  bool raw = halTouch(&x, &y);

  // Debounce the RELEASE edge only. A momentary empty read mid-press used to
  // read as a release: that fired a tap, then the next poll saw the finger
  // again and started a fresh press. Requiring consecutive empty polls before
  // believing a release costs one frame of latency and makes a single press
  // produce a single tap.
  if (raw) {
    _upPolls = 0;
  } else if (_touchDown && _upPolls < TOUCH_UP_POLLS) {
    _upPolls++;
    raw = true;              // still held as far as the gesture layer knows
  }

  bool down = raw;
  if (down && (_upPolls == 0)) { _tx = x; _ty = y; }   // keep last good coords
  _touchDown = down;

  updateBodyTouch(now);

  bool inStrip = down && _ty >= STRIP_Y;
  M5.BtnA.feed(inStrip && _tx < SCREEN_W / 2, now);
  M5.BtnB.feed((inStrip && _tx >= SCREEN_W / 2) || bodyTouchZone(2) > 0, now);

  // Gestures start ANYWHERE on the panel, including the button strip. A
  // swipe is a whole-hand motion; requiring it to begin above the strip
  // meant it silently failed from wherever the finger happened to land.
  // Taps in the strip still work as buttons: the button state machine runs
  // normally and is only withdrawn (cancel()) if the press becomes a swipe.
  bool inPet = down;
  if (inPet && !_petTouch) {              // touch-down anywhere
    _petTouch = true;
    _petDownMs = now; _petDownX = _tx; _petDownY = _ty;
    _lastX = _tx; _dir = 0; _reversals = 0; _swipeFired = false;
    bodyNoteToucher(_tx < SCREEN_W / 2 ? -1 : 1);
  } else if (down && _petTouch) {         // drag continues
    // A steady 600ms press becomes a hold (push-to-talk); once a hold is
    // active, scrub detection is off — finger wobble mid-dictation must
    // not fire dizzy.
    if (!_holdActive && now - _petDownMs >= 600
        && abs(_tx - _petDownX) < 20 && abs(_ty - _petDownY) < 20) {
      _holdActive = true;
      _evHoldStart = true;
      M5.BtnA.cancel();
      M5.BtnB.cancel();
    }
    if (!_holdActive) {
      // Downward swipe → Enter. Checked before scrub so a deliberate vertical
      // drag can't accumulate reversals and read as a scrub instead. Mostly
      // vertical (2:1) so a diagonal flick during a scrub doesn't fire it.
      // 45px: the only other pet gesture is a HOLD, which is stationary, so
      // a downward drag has nothing to be confused with.
      int dyTot = _ty - _petDownY;
      int dxTot = abs(_tx - _petDownX);
      if (!_swipeFired && dyTot > 45 && dyTot > dxTot * 2) {
        _evSwipeDown = true;
        _swipeFired = true;      // latch: one Enter per press
        _reversals = 0;          // and it is definitively not a scrub
        // The gesture consumes this press — a swipe that began in (or ended
        // in) the button strip must not also cycle screens on lift.
        M5.BtnA.cancel();
        M5.BtnB.cancel();
      }
      // Horizontal swipe → option navigation. Mostly-horizontal by the same
      // 2:1 ratio, slightly longer travel than swipe-down so a lazy diagonal
      // Enter-swipe can't misread as navigation.
      int dxTotS = _tx - _petDownX;
      if (!_swipeFired && abs(dxTotS) > 50 && abs(dxTotS) > abs(dyTot) * 2) {
        if (dxTotS > 0) _evSwipeRight = true; else _evSwipeLeft = true;
        _swipeFired = true;
        _reversals = 0;
        M5.BtnA.cancel();
        M5.BtnB.cancel();
      }
      if (!_swipeFired) {
        int dx = _tx - _lastX;
        if (abs(dx) > 12) {
          int8_t d = dx > 0 ? 1 : -1;
          if (_dir != 0 && d != _dir) _reversals++;
          _dir = d;
          _lastX = _tx;
        }
        if (_reversals >= 3) { _evScrub = true; _petTouch = false; }
      }
    }
  } else if (!down && _petTouch) {        // release
    _petTouch = false;
    if (_holdActive) {
      _holdActive = false;
      _evHoldEnd = true;                  // hold consumed the press: no tap
    } else if (!_swipeFired) {
      uint32_t held = now - _petDownMs;
      if (held < 450 && abs(_tx - _petDownX) < 20 && abs(_ty - _petDownY) < 20) {
        _evTap = true;
        _tapFromBody = false;
      }
    }
  }
  // A gesture that STARTS on the pet keeps tracking wherever the finger
  // goes; there is deliberately no "finger left the pet zone" branch.
  // Ending the gesture is the release branch's job alone.
}

bool M5Compat::petTapped()   { bool e = _evTap;   _evTap = false;   return e; }
bool M5Compat::petScrubbed() { bool e = _evScrub; _evScrub = false; return e; }
bool M5Compat::petHoldStarted() { bool e = _evHoldStart; _evHoldStart = false; return e; }
bool M5Compat::petHoldEnded()   { bool e = _evHoldEnd;   _evHoldEnd = false;   return e; }
bool M5Compat::petSwipedDown()  { bool e = _evSwipeDown; _evSwipeDown = false; return e; }
bool M5Compat::petSwipedLeft()  { bool e = _evSwipeLeft; _evSwipeLeft = false; return e; }
bool M5Compat::petSwipedRight() { bool e = _evSwipeRight; _evSwipeRight = false; return e; }
