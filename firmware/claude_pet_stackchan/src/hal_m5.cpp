// CoreS3 core peripherals. This TU and body.cpp are the only ones that see
// M5Unified's global `M5`; see hal_m5.h for why.
#include "hal_m5.h"
#include <M5StackChan.h>
#include <sys/time.h>
#include <time.h>

int bodyServoPitchTenthsToCode(int tenths);   // body.cpp: the saved pitch-axis mirror (pitchRev) for a raw servo read

void halBegin() {
  // Enlarge the HWCDC rings before the first begin(): the bridge's
  // heartbeats arrive as one USB-speed burst, and the default 256-byte RX
  // ring was the leading suspect for the one-way wedge on the Freenove
  // build. HWCDC::begin() only allocates when a ring is NULL, so the second
  // Serial.begin() inside M5.begin() keeps these sizes.
  Serial.setRxBufferSize(4096);
  Serial.setTxBufferSize(4096);
  Serial.setTxTimeoutMs(20);
  Serial.begin(115200);
  // M5StackChan.begin() calls M5.begin() itself (default config: display,
  // touch, speaker, RTC, power), then brings up the PY32 expander (servo
  // rail on, 12 LEDs), the Feetech bus + Motion task, and the INA226.
  M5StackChan.begin();
  // Quiet by default: the CoreS3 external 5 V converter whines at idle on
  // this robot. Its rail supplies rear LEDs / top touch, not the head motors.
  M5.Power.setExtOutput(false);
  Serial.printf("[power] quiet default: external_5v=%d\n", M5.Power.getExtOutput());
  // begin() enables the CoreS3 amplifier. Keep it off through setup and
  // between chirps; playRaw() starts the speaker again on demand.
  M5.Speaker.end();
  // The pet renders a 320x240 landscape frame. M5GFX picks the panel's
  // native orientation for CoreS3; only correct it if that came up portrait.
  if (M5.Display.width() < M5.Display.height()) {
    M5.Display.setRotation(M5.Display.getRotation() ^ 1);
  }
  M5.Display.fillScreen(TFT_BLACK);
  M5.Speaker.setVolume(96);
}

// The top Si12T sensor is on the disabled auxiliary rail. Keep updating
// CoreS3 screen touch/buttons without polling that unpowered sensor.
void halUpdate() {
  M5.update();
  if (M5.Power.getExtOutput()) M5StackChan.TouchSensor.update();
}

namespace {
enum class NoiseTest { None, Screen, Motors, External, Speaker };
NoiseTest noiseTest = NoiseTest::None;
uint32_t noiseUntil = 0;
uint8_t savedBrightness = 0;
bool savedExternal = false;

void restoreNoiseTest() {
  switch (noiseTest) {
    case NoiseTest::Screen: M5.Display.setBrightness(savedBrightness); break;
    case NoiseTest::Motors: M5StackChan.setServoPowerEnabled(true); break;
    case NoiseTest::External: M5.Power.setExtOutput(savedExternal); break;
    // Speaker.end() is the normal idle state; the next chirp restarts it.
    default: break;
  }
  if (noiseTest != NoiseTest::None) Serial.println("[noise] restored");
  noiseTest = NoiseTest::None;
}
}

bool halNoiseTest(const char* target) {
  NoiseTest next;
  if (!strcmp(target, "restore")) next = NoiseTest::None;
  else if (!strcmp(target, "screen")) next = NoiseTest::Screen;
  else if (!strcmp(target, "motors")) next = NoiseTest::Motors;
  else if (!strcmp(target, "external")) next = NoiseTest::External;
  else if (!strcmp(target, "speaker")) next = NoiseTest::Speaker;
  else return false;
  restoreNoiseTest();
  noiseTest = next;
  noiseUntil = millis() + 45000;
  switch (next) {
    case NoiseTest::Screen:
      savedBrightness = M5.Display.getBrightness();
      M5.Display.setBrightness(0);
      break;
    case NoiseTest::Motors:
      M5StackChan.Motion.setTorqueEnabled(false);
      M5StackChan.setServoPowerEnabled(false);
      break;
    case NoiseTest::External:
      savedExternal = M5.Power.getExtOutput();
      M5.Power.setExtOutput(false);
      Serial.printf("[noise] external before=%d after=%d\n", savedExternal, M5.Power.getExtOutput());
      break;
    case NoiseTest::Speaker:
      M5.Speaker.end();
      break;
    default: break;
  }
  Serial.printf("[noise] target=%s timeout_ms=%u\n", target, next == NoiseTest::None ? 0 : 45000);
  return true;
}

bool halNoiseMotorsOff() { return noiseTest == NoiseTest::Motors; }

bool halExtPower(bool on, int* yaw, int* pitch) {
  M5.Power.setExtOutput(on);
  delay(300);
  *yaw = M5StackChan.Motion.getCurrentXAngle();
  *pitch = bodyServoPitchTenthsToCode(M5StackChan.Motion.getCurrentYAngle());   // code convention, as body.cpp logs it
  bool now = M5.Power.getExtOutput();
  Serial.printf("[power] external_5v=%d servo yaw=%d pitch=%d (0.1deg)\n", now, *yaw, *pitch);
  return now == on;
}

void halNoiseTestUpdate() {
  if (noiseTest == NoiseTest::None) return;
  if ((int32_t)(millis() - noiseUntil) >= 0) { restoreNoiseTest(); return; }
  // A wake gesture or brightness policy must not invalidate a screen test.
  if (noiseTest == NoiseTest::Screen && M5.Display.getBrightness()) M5.Display.setBrightness(0);
  if (noiseTest == NoiseTest::Speaker && M5.Speaker.isRunning()) M5.Speaker.end();
}

M5GFX& halDisplay() { return M5.Display; }

bool halTouch(int* x, int* y) {
  if (M5.Touch.getCount() == 0) return false;
  auto& d = M5.Touch.getDetail();
  if (!d.isPressed()) return false;
  *x = d.x; *y = d.y;
  return true;
}

void halBrightness(uint8_t level) { M5.Display.setBrightness(level); }

int  halBatteryMv() { return M5.Power.getBatteryVoltage(); }
int  halBatteryMa() { return M5.Power.getBatteryCurrent(); }
int  halVbusMv()    { return M5.Power.getVBUSVoltage(); }
bool halPowerClicked() { return M5.BtnPWR.wasClicked(); }
void halDeepSleep() {
  M5.Display.setBrightness(0);
  M5.Power.deepSleep(0, true);
}

void halPowerOff() {
  M5.Display.setBrightness(0);
  M5.Power.powerOff();
}

// data.h pushes *local* time components (epoch already tz-adjusted). Store
// them as-is in the RTC and mirror them into the system clock as "UTC" so
// time()/gmtime_r agree with what the RTC reads back.
void halRtcSet(int h, int m, int s, int wday, int mon, int mday, int year) {
  struct tm t = {};
  t.tm_hour = h; t.tm_min = m; t.tm_sec = s;
  t.tm_mday = mday; t.tm_mon = mon - 1; t.tm_year = year - 1900; t.tm_wday = wday;
  if (M5.Rtc.isEnabled()) {
    m5::rtc_date_t d; d.year = year; d.month = mon; d.date = mday; d.weekDay = wday;
    m5::rtc_time_t tt(h, m, s);
    M5.Rtc.setDateTime(&d, &tt);
  }
  time_t epoch = mktime(&t);   // TZ unset: interpreted as UTC
  struct timeval tv = { .tv_sec = epoch, .tv_usec = 0 };
  settimeofday(&tv, nullptr);
}

void halRtcGet(int* h, int* m, int* s, int* wday, int* mon, int* mday, int* year) {
  if (M5.Rtc.isEnabled()) {
    m5::rtc_date_t d; m5::rtc_time_t t;
    if (M5.Rtc.getDateTime(&d, &t)) {
      *h = t.hours; *m = t.minutes; *s = t.seconds;
      *wday = d.weekDay; *mon = d.month; *mday = d.date; *year = d.year;
      return;
    }
  }
  time_t now = time(nullptr);
  struct tm lt; gmtime_r(&now, &lt);
  *h = lt.tm_hour; *m = lt.tm_min; *s = lt.tm_sec;
  *wday = lt.tm_wday; *mon = lt.tm_mon + 1; *mday = lt.tm_mday; *year = lt.tm_year + 1900;
}
