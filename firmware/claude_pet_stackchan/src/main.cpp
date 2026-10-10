#include "board_compat.h"
#include "hal_m5.h"   // halDisplay(): the panel address without touching M5.Lcd at static init
#include <LittleFS.h>
#include <stdarg.h>
#include "ble_bridge.h"
#include "data.h"
#include "hostlook.h"
#include "eve.h"

// Parent must come from halDisplay(), not &M5.Lcd: M5.Lcd is a reference
// member of pet::M5, which is constructed in another TU. Static-init order
// is unspecified across TUs, so reading it here handed the sprite a null
// parent and the first pushSprite() in setup() died with LoadProhibited
// (boot loop on the first StackChan flash, 2026-09-05). halDisplay() returns
// the M5Unified global by address, which is a link-time constant.
TFT_eSprite spr = TFT_eSprite(&halDisplay());

// Advertise as "Claude-XXXX" (last two BT MAC bytes) so multiple sticks
// in one room are distinguishable in the desktop picker. Name persists in
// btName for the BLUETOOTH info page.
static char btName[16] = "Claude";
static void startBt() {
  uint8_t mac[6] = {0};
  esp_read_mac(mac, ESP_MAC_BT);
  snprintf(btName, sizeof(btName), "Claude-%02X%02X", mac[4], mac[5]);
  bleInit(btName);
}

#include "character.h"
#include "stats.h"
#include "persona.h"
#include "face.h"
#include "ticks.h"
#include "body.h"
#include "motion.h"
#include "eyes.h"
#include "chirp.h"
#include "gaze.h"
#include "look.h"
#include "mood.h"
// Landscape 320x240 (the robot's face). Portrait build was 240x320.
const int W = SCREEN_W, H = SCREEN_H;
const int CX = W / 2;
const int CY_BASE = 120;

#ifndef CLAUDE_PET_GIT_SHA
#define CLAUDE_PET_GIT_SHA "dev"   // flash script may pass -DCLAUDE_PET_GIT_SHA=\"abc123\"
#endif

// Colors used across multiple UI surfaces
const uint16_t HOT   = 0xFA20;   // red-orange: warnings, impatience, deny
const uint16_t PANEL = 0x2104;   // overlay panel background

// PersonaState enum lives in persona.h (shared with body.cpp).
const char* stateNames[] = { "sleep", "idle", "busy", "attention", "celebrate", "dizzy", "heart" };

TamaState    tama;
PersonaState baseState   = P_SLEEP;
PersonaState activeState = P_SLEEP;
uint32_t     oneShotUntil = 0;
uint32_t     lastShakeCheck = 0;
float        accelBaseline = 1.0f;
unsigned long t = 0;

uint8_t brightLevel = 4;           // 0..4 → ScreenBreath 20..100

uint8_t msgScroll = 0;
uint16_t lastLineGen = 0;
uint32_t lastInteractMs = 0;
bool     dimmed = false;
bool     screenOff = false;
bool     swallowBtnB = false;
bool     eveMode = false;
bool     gifAvailable = false;
const uint8_t SPECIES_GIF = 0xFF;   // species NVS sentinel: use the installed GIF
uint32_t wakeTransitionUntil = 0;
const uint32_t SCREEN_OFF_MS = 30000;

bool     napping = false;
uint32_t napStartMs = 0;

// Face-down = Z-axis dominant and negative. Debounced so a toss doesn't count.
static bool isFaceDown() {
  float ax, ay, az;
  M5.Imu.getAccelData(&ax, &ay, &az);
  return az < -0.7f && fabsf(ax) < 0.4f && fabsf(ay) < 0.4f;
}

static void applyBrightness() { M5.Axp.ScreenBreath(20 + brightLevel * 20); }

static void wake() {
  lastInteractMs = millis();
  if (screenOff) {
    M5.Axp.SetLDO2(true);
    applyBrightness();
    screenOff = false;
    wakeTransitionUntil = millis() + 12000;
  }
  if (dimmed) { applyBrightness(); dimmed = false; }
}

static void beep(uint16_t freq, uint16_t dur) {
  if (settings().sound) M5.Beep.tone(freq, dur);
}

static void sendCmd(const char* json) {
  Serial.println(json);
  size_t n = strlen(json);
  bleWrite((const uint8_t*)json, n);
  bleWrite((const uint8_t*)"\n", 1);
}

// Full repaint: clear the sprite and let the character/eve redraw from
// scratch on the next tick. Used when an overlay (card, passkey) goes away.
static void repaintAll() {
  spr.fillSprite(0x0000);
  characterInvalidate();
}

// The on-device menu (and its settings/reset panels) is gone — customization
// lives host-side in the CLI (`eve-bridge species`, matchers.toml), and
// a 2.8" panel poked with a fingertip was a bad settings UI that suspended
// the gestures that are the point of the device. Settings persist in NVS and
// keep whatever values they last had; the structs stay for the render paths
// that read them.

// Clock orientation: gravity along the in-plane X axis means the stick is
// on its side. Signed counter for hysteresis on both transitions — same
// pattern as face-down nap.
//   0 = portrait (sprite path, pet sleeps underneath)
//   1 = landscape, BtnA-side down (M5.Lcd rotation 1)
//   3 = landscape, USB-side down (M5.Lcd rotation 3)
static uint8_t clockOrient   = 0;
static int8_t  orientFrames  = 0;
static uint8_t paintedOrient = 0;
// RTC and IMU share an I2C bus. Reading the RTC at 60fps starves the IMU
// reads in clockUpdateOrient — orientation detection gets noisy. Cache the
// time once per second; mood logic and drawClock both read from here.
static RTC_TimeTypeDef _clkTm;
static RTC_DateTypeDef _clkDt;
uint32_t               _clkLastRead = 0;   // zeroed by data.h on time-sync
static bool            _onUsb       = false;
static void clockRefreshRtc() {
  if (millis() - _clkLastRead < 1000) return;
  _clkLastRead = millis();
  _onUsb = M5.Axp.GetVBusVoltage() > 4.0f;
  M5.Rtc.GetTime(&_clkTm);
  M5.Rtc.GetDate(&_clkDt);
}

// Power follows the USB port (owner, 2026-09-23: "only take power from the port, not the battery").
// The body battery stays in circuit because the servo rail is boosted from BAT+, but the robot never
// runs on it: once USB power has been gone for USB_LOSS_OFF_MS, the AXP2101 cuts power. That also
// makes a host restart a clean reboot for the robot. On 2026-09-23 the Mac restarted, the robot kept
// running on its battery and came back wedged, deaf to every reset but a power cycle.
// Armed only after USB power has been seen once since boot, so a VBUS misread can never power the
// robot off right after it starts.
static constexpr uint32_t USB_LOSS_OFF_MS = 5000;
static bool     _usbSeen   = false;
static uint32_t _usbLostAt = 0;
static void powerFollowUsb() {
  if (_onUsb) { _usbSeen = true; _usbLostAt = 0; return; }
  if (!_usbSeen) return;
  uint32_t now = millis();
  if (_usbLostAt == 0) { _usbLostAt = now; return; }
  if (now - _usbLostAt < USB_LOSS_OFF_MS) return;
  Serial.println("[power] USB power gone for 5 s: powering off");
  halPowerOff();
}

static void clockUpdateOrient() {
  float ax, ay, az;
  M5.Imu.getAccelData(&ax, &ay, &az);
  uint8_t lock = settings().clockRot;
  if (lock == 1) { clockOrient = 0; return; }
  if (lock == 2) {
    // Locked landscape: never drop to 0, but still pick 1 vs 3 from
    // gravity so the cradle works either way up. Need a strong tilt
    // for the 1↔3 swap so handling jitter doesn't flip it; otherwise
    // hold whatever we last had (or 1 from boot).
    if (clockOrient == 0) clockOrient = (ax >= 0) ? 1 : 3;
    if      (ax >  0.5f && clockOrient != 1) clockOrient = 1;
    else if (ax < -0.5f && clockOrient != 3) clockOrient = 3;
    return;
  }
  // Dual threshold: strict to enter (must be clearly sideways), loose to
  // stay (tolerate ~65° of tilt). With one shared threshold a slight lean
  // while sitting on the long edge puts ax right at the boundary and the
  // counter ratchets down in ~half a second.
  bool side = (clockOrient == 0)
    ? fabsf(ax) > 0.7f && fabsf(ay) < 0.5f && fabsf(az) < 0.5f
    : fabsf(ax) > 0.4f;
  if (side) { if (orientFrames < 20) orientFrames++; }
  else      { if (orientFrames > -10) orientFrames--; }
  if (clockOrient == 0 && orientFrames >= 15) {
    clockOrient = (ax > 0) ? 1 : 3;
  } else if (clockOrient != 0 && orientFrames <= -8) {
    clockOrient = 0;
  } else if (clockOrient != 0 && side) {
    // Direct 1↔3: a fast flip keeps |ax|>0.7 (just changes sign), so
    // `side` never drops and the exit-via-0 path can't fire. Watch for
    // ax sign disagreeing with the stored orientation.
    static int8_t swapFrames = 0;
    uint8_t want = (ax > 0) ? 1 : 3;
    if (want != clockOrient) { if (++swapFrames >= 8) { clockOrient = want; swapFrames = 0; } }
    else swapFrames = 0;
  }
}

// Clock face: shown when charging on USB with nothing else going on.
// Portrait paints the upper ~110px to the sprite; pet renders below.
// Landscape draws direct to LCD with rotation — sprite stays untouched.
static const char* const MON[] = {
  "Jan","Feb","Mar","Apr","May","Jun","Jul","Aug","Sep","Oct","Nov","Dec"
};
static const char* const DOW[] = {"Sun","Mon","Tue","Wed","Thu","Fri","Sat"};

static uint8_t clockDow() { return _clkDt.WeekDay % 7; }

// 12-hour face. The hour is space-padded rather than zero-padded so the
// string keeps a constant width: both faces centre the time with MC_DATUM
// and only repaint their own glyph cells, so a 5→4 char shrink (12:59 → 1:00)
// would strand pixels. A leading space is drawn as a background-filled cell,
// which clears them. Mood logic below still reads the raw 24h _clkTm.Hours.
static uint8_t clockHour12() {
  uint8_t h = _clkTm.Hours % 12;
  return h == 0 ? 12 : h;
}
static const char* clockMeridiem() { return _clkTm.Hours < 12 ? "AM" : "PM"; }

// Small always-on clock, top-left of the normal pet screen. Drawn LAST each
// frame (just before pushSprite) because the eve strip clears y<126 full
// width — anything painted earlier in the corner is erased. Overlap with the
// pet's overlay particles (hearts/Zzz) is possible and harmless; the clock
// wins for one frame and the particles are transient.
[[maybe_unused]] static void drawMiniClock() {   // not called on the robot's face
  if (!dataRtcValid()) return;
  const Palette& p = characterPalette();
  char mc[12];
  snprintf(mc, sizeof(mc), "%u:%02u %s", clockHour12(), _clkTm.Minutes, clockMeridiem());
  spr.setTextSize(2);
  spr.setTextColor(p.textDim, p.bg);
  spr.setCursor(4, 4);
  spr.print(mc);
  spr.setTextSize(1);
}

static void drawClock() {
  const Palette& p = characterPalette();
  char hm[6]; snprintf(hm, sizeof(hm), "%2u:%02u", clockHour12(), _clkTm.Minutes);
  char ss[8]; snprintf(ss, sizeof(ss), ":%02u %s", _clkTm.Seconds, clockMeridiem());
  uint8_t mi = (_clkDt.Month >= 1 && _clkDt.Month <= 12) ? _clkDt.Month - 1 : 0;
  char dl[8]; snprintf(dl, sizeof(dl), "%s %02u", MON[mi], _clkDt.Date);

  if (clockOrient == 0) {
    paintedOrient = 0;
    // Bottom half — eve naturally lives at y=0..82, GIF peeks at top
    // via peek mode. Clearing from 90 leaves both untouched.
    spr.fillRect(0, 90, W, H - 90, p.bg);
    spr.setTextDatum(MC_DATUM);
    spr.setTextSize(4); spr.setTextColor(p.text, p.bg);    spr.drawString(hm, CX, 140);
    spr.setTextSize(2); spr.setTextColor(p.textDim, p.bg); spr.drawString(ss, CX, 175);
    spr.setTextSize(1);                                     spr.drawString(dl, CX, 200);
    spr.setTextDatum(TL_DATUM);
    return;
  }

  // Landscape: 240×135 direct-to-LCD. Full fill only on entry; after that
  // text glyph bg cells repaint themselves and the pet box (small, ~90×50)
  // gets a fillRect each pet tick — small enough not to tear.
  M5.Lcd.setRotation(clockOrient);
  static uint8_t lastSec = 0xFF;
  bool repaint = paintedOrient != clockOrient;
  if (repaint) { M5.Lcd.fillScreen(p.bg); paintedOrient = clockOrient; lastSec = 0xFF; }

  // Seconds tick at 1Hz; redrawing 3 strings at 60fps is 180 SPI ops/sec
  // for nothing. Gate on the second changing (or full repaint).
  if (repaint || _clkTm.Seconds != lastSec) {
    lastSec = _clkTm.Seconds;
    char wdl[12]; snprintf(wdl, sizeof(wdl), "%s %s %02u", DOW[clockDow()], MON[mi], _clkDt.Date);
    char ssl[7]; snprintf(ssl, sizeof(ssl), "%02u %s", _clkTm.Seconds, clockMeridiem());
    M5.Lcd.setTextDatum(MC_DATUM);
    M5.Lcd.setTextSize(3); M5.Lcd.setTextColor(p.text, p.bg);    M5.Lcd.drawString(hm, 170, 42);
    M5.Lcd.setTextSize(2); M5.Lcd.setTextColor(p.textDim, p.bg); M5.Lcd.drawString(ssl, 170, 72);
                                                                  M5.Lcd.drawString(wdl, 170, 102);
    M5.Lcd.setTextDatum(TL_DATUM);
    M5.Lcd.setTextSize(1);
  }

  // Pet on left at 5 fps. Clear includes the overlay-particle zone above
  // the body (y<30) — species draw Zzz/hearts there via EVE_Y_OVERLAY=6
  // which doesn't go through _yb, so the box has to cover it.
  static uint32_t lastPetTick = 0;
  if (millis() - lastPetTick >= 200) {
    lastPetTick = millis();
    if (eveMode) {
      // ASCII glyphs don't self-clear; wipe the box each tick. Species
      // hardcode EVE_X_CENTER=67 / EVE_Y_OVERLAY=6 for particles so
      // keep portrait coords and just swap the surface — pet lands
      // upper-left of landscape, which is where we want it anyway.
      M5.Lcd.fillRect(0, 0, 115, 90, p.bg);
      eveRenderTo(&M5.Lcd, activeState);
    } else {
      // Full-frame GIFs paint every pixel (transparent → pal.bg), so a
      // per-tick clear just adds a visible black flash between wipe and
      // last scanline. The entry fillScreen on paintedOrient change
      // already covers the surround.
      characterSetState(activeState);
      characterRenderTo(&M5.Lcd, 57, 45);
    }
  }
  M5.Lcd.setRotation(0);
}

// Robot mapping (owner's gold standard): three states must read at a glance
// on the body as well as the screen. Claude idle → the pet SLEEPS; Claude
// working (any session running) → BUSY; a session blocked on a permission →
// ATTENTION. The Freenove build needed 3 running sessions for busy and kept
// an idle-but-awake pet; here "awake and idle" is reserved for no daemon.
PersonaState derive(const TamaState& s) {
  if (!s.connected)            return P_IDLE;   // no daemon: awake, looking around
  if (s.sessionsWaiting > 0)   return P_ATTENTION;
  if (s.recentlyCompleted)     return P_CELEBRATE;
  if (s.sessionsRunning >= 1)  return P_BUSY;
  return P_SLEEP;  // connected, nothing running — Claude is idle, pet naps
}

void triggerOneShot(PersonaState s, uint32_t durMs) {
  activeState = s;
  oneShotUntil = millis() + durMs;
}

bool checkShake() {
  float ax, ay, az;
  M5.Imu.getAccelData(&ax, &ay, &az);
  float mag = sqrtf(ax*ax + ay*ay + az*az);
  float delta = fabsf(mag - accelBaseline);
  accelBaseline = accelBaseline * 0.95f + mag * 0.05f;
  return delta > 0.8f;
}




void drawPasskey() {
  const Palette& p = characterPalette();
  spr.fillSprite(p.bg);
  spr.setTextSize(1);
  spr.setTextColor(p.textDim, p.bg);
  spr.setCursor(8, 56);  spr.print("BLUETOOTH PAIRING");
  spr.setCursor(8, 184); spr.print("enter on desktop:");
  spr.setTextSize(3);
  spr.setTextColor(p.text, p.bg);
  char b[8]; snprintf(b, sizeof(b), "%06lu", (unsigned long)blePasskey());
  spr.setCursor((W - 18 * 6) / 2, 110);
  spr.print(b);
}

// Greedy word-wrap into fixed-width rows. Continuation rows get a leading
// space. Returns number of rows written.
static uint8_t wrapInto(const char* in, char out[][40], uint8_t maxRows, uint8_t width) {
  uint8_t row = 0, col = 0;
  const char* p = in;
  while (*p && row < maxRows) {
    while (*p == ' ') p++;                     // skip leading spaces
    // measure next word
    const char* w = p;
    while (*p && *p != ' ') p++;
    uint8_t wlen = p - w;
    if (wlen == 0) break;
    uint8_t need = (col > 0 ? 1 : 0) + wlen;
    if (col + need > width) {
      out[row][col] = 0;
      if (++row >= maxRows) return row;
      out[row][0] = ' '; col = 1;              // continuation indent
    }
    if (col > 1 || (col == 1 && out[row][0] != ' ')) out[row][col++] = ' ';
    else if (col == 1 && row > 0) {}           // already have the indent space
    // hard-break words that still don't fit
    while (wlen > width - col) {
      uint8_t take = width - col;
      memcpy(&out[row][col], w, take); col += take; w += take; wlen -= take;
      out[row][col] = 0;
      if (++row >= maxRows) return row;
      out[row][0] = ' '; col = 1;
    }
    memcpy(&out[row][col], w, wlen); col += wlen;
  }
  if (col > 0 && row < maxRows) { out[row][col] = 0; row++; }
  return row;
}

void drawHUD() {
  const Palette& p = characterPalette();
  // 4 rows on the landscape panel: y 204..240 (STRIP_Y), under the pet.
  const int SHOW = 4, LH = 8, WIDTH = 38;
  const int AREA = SHOW * LH + 4;
  spr.fillRect(0, H - AREA, W, AREA, p.bg);
  spr.setTextSize(1);

  if (tama.lineGen != lastLineGen) { msgScroll = 0; lastLineGen = tama.lineGen; wake(); }

  if (tama.nLines == 0) {
    spr.setTextColor(p.text, p.bg);
    spr.setCursor(4, H - LH - 2);
    spr.print(tama.msg);
    return;
  }

  // Wrap all transcript lines into a flat display buffer. Track which
  // transcript index each display row came from, so we can dim older ones.
  static char disp[32][40];
  static uint8_t srcOf[32];
  uint8_t nDisp = 0;
  for (uint8_t i = 0; i < tama.nLines && nDisp < 32; i++) {
    uint8_t got = wrapInto(tama.lines[i], &disp[nDisp], 32 - nDisp, WIDTH);
    for (uint8_t j = 0; j < got; j++) srcOf[nDisp + j] = i;
    nDisp += got;
  }

  uint8_t maxBack = (nDisp > SHOW) ? (nDisp - SHOW) : 0;
  if (msgScroll > maxBack) msgScroll = maxBack;

  int end = (int)nDisp - msgScroll;
  int start = end - SHOW; if (start < 0) start = 0;
  uint8_t newest = tama.nLines - 1;
  for (int i = 0; start + i < end; i++) {
    uint8_t row = start + i;
    bool fresh = (srcOf[row] == newest) && (msgScroll == 0);
    spr.setTextColor(fresh ? p.text : p.textDim, p.bg);
    spr.setCursor(4, H - AREA + 2 + i * LH);
    spr.print(disp[row]);
  }
  if (msgScroll > 0) {
    spr.setTextColor(p.body, p.bg);
    spr.setCursor(W - 18, H - LH - 2);
    spr.printf("-%u", msgScroll);
  }
}

void setup() {
  // FIRST: capture the reset reason and the pre-reset event ring before any
  // init can crash. Everything after this point is diagnosable.
  diagInit();
  // Arm the watchdog before the slow parts of setup, not after: a hang
  // during init used to leave the board silent forever while the USB port
  // kept enumerating. Setup's delays total ~3.8s against the 15s budget.
  diagWatchdogBegin();
  // Phase + ring checkpoints through setup: the 2026-08-05 12:37 plug-in
  // crash (INT-WDT in the systick handler, systimer never latching) reported
  // "DIED IN: none" because nothing ever set a phase before loop(). Every
  // setup death now names the stage it died after.
  diagPhase(DP_SETUP);
  M5.begin();
  diagLog("m5.begin done");
  delay(2000);                       // let the host attach before first prints
  diagReport("boot");                // why the last run ended + what it was doing
  Serial.println("[boot] claude_pet_stackchan " CLAUDE_PET_GIT_SHA);
  Serial.println("[boot] M5.begin done");
  // Measured panel, so the layout below rests on real numbers.
  Serial.printf("[boot] display %dx%d rot=%d\n",
                (int)M5.Lcd.width(), (int)M5.Lcd.height(), (int)M5.Lcd.getRotation());
  // Rotation is fixed landscape by halBegin(); no setRotation here.
  M5.Imu.Init();
  M5.Beep.begin();
  chirpBegin();                      // R2D2 phrase buffers in PSRAM
  bodyBegin();                       // servo rail on, goHome, LEDs dark
  diagLog("body up");
  gazeBegin();                       // owner memory from NVS + camera task (core 0)
  diagLog("gaze up");
  startBt();
  diagLog("bt up");
  applyBrightness();
  Serial.println("[boot] body+bt+brightness done");
  lastInteractMs = millis();
  statsLoad();
  settingsLoad();
  petNameLoad();
  eveInit();

  // BLE stays always-on; s.bt is stored as a preference only.
  // 320x240x16 = 150 KB: put the frame in QSPI PSRAM (LGFX_Sprite defaults
  // to internal RAM, which would fail or starve the heap).
  spr.setPsram(true);
  spr.createSprite(W, H);
  eyesBegin();                       // RoboEyes canvas, pushed into spr each frame
  Serial.printf("[boot] sprite created=%d psram=%d heap=%u\n",
                (int)spr.created(), (int)psramFound(), ESP.getFreeHeap());
  characterInit(nullptr);  // scan /characters/ for whatever is installed
  diagLog("characterInit done");
  Serial.println("[boot] characterInit done");
  gifAvailable = characterLoaded();
  {
    const Palette& p = characterPalette();
    eyesSetBackground(p.bg);         // eye colour is random per boot (eyes.cpp)
  }
  // species NVS: 0..N-1 = ASCII species, 0xFF = use GIF (also the default,
  // so a fresh install lands on the GIF). With no GIF installed, 0xFF falls
  // through to eveInit()'s clamped default.
  eveMode = !(gifAvailable && speciesIdxLoad() == SPECIES_GIF);
  repaintAll();

  {
    const Palette& p = characterPalette();
    spr.fillSprite(p.bg);
    spr.setTextDatum(MC_DATUM);
    spr.setTextSize(2);
    if (ownerName()[0]) {
      char line[40];
      snprintf(line, sizeof(line), "%s's", ownerName());
      spr.setTextColor(p.text, p.bg);   spr.drawString(line, W/2, H/2 - 12);
      spr.setTextColor(p.body, p.bg);   spr.drawString(petName(), W/2, H/2 + 12);
    } else {
      // First boot, no owner pushed yet — say hi.
      spr.setTextColor(p.body, p.bg);   spr.drawString("Hello!", W/2, H/2 - 12);
      spr.setTextSize(1);
      spr.setTextColor(p.textDim, p.bg);
      spr.drawString("an eve appears", W/2, H/2 + 12);
    }
    spr.setTextDatum(TL_DATUM); spr.setTextSize(1);
    spr.pushSprite(0, 0);
    delay(1800);
  }

  Serial.printf("eve: %s\n", eveMode ? "ASCII mode" : "GIF character loaded");
  diagLog("setup done eve=%d", (int)eveMode);
}

// The mood expression the eyes render this frame (nullptr = not exploring).
static const MoodExpr* moodExprForEyes = nullptr;
static expression::State semanticExpression;

void loop() {
  diagWatchdogFeed();   // a hang past DIAG_WDT_SECS now resets + reports
  static uint32_t _loopStart = 0;
  if (_loopStart) diagLoopEnd(millis() - _loopStart);
  _loopStart = millis();
  diagLoopTick();
  diagPhase(DP_TOUCH);
  M5.update();
  M5.Beep.update();
  t++;
  uint32_t now = millis();
  static uint32_t aliveAt = 0;
  if ((int32_t)(now - aliveAt) >= 0) {
    aliveAt = now + 5000;
    Serial.printf("[alive] up=%lus heap=%u state=%s\n",
                  now / 1000, ESP.getFreeHeap(), stateNames[activeState]);
  }

  diagPhase(DP_DATA);
  dataPoll(&tama);
  if (tama.expressionPending) {
    semanticExpression.accept(tama.expressionRequest, millis());
    tama.expressionPending = false;
  }
  diagPhase(DP_STATS);
  if (statsPollLevelUp()) triggerOneShot(P_CELEBRATE, 3000);
  baseState = derive(tama);

  // After waking the screen, hold sleep for 12s so users see the wake-up
  // animation. Urgent states (attention, celebrate, busy) override this.
  if (baseState == P_IDLE && (int32_t)(now - wakeTransitionUntil) < 0) baseState = P_SLEEP;

  if ((int32_t)(now - oneShotUntil) >= 0) activeState = baseState;

  diagPhase(DP_LED);
  // Body: head servos + the 12 LEDs follow activeState (see body.cpp for the
  // choreography table). Solid blue overrides every LED colour while the host
  // is listening — it is the "mic is live" indicator.
  // needsAttention is the BASE state so a one-shot overlay (level-up
  // celebrate) cannot drop the head while a session is still waiting.
  // Listening pose: the host's dictation key ({"cmd":"listen"}) is the only
  // source — face the user, mic-blue LEDs, "listening..." balloon.
  // A touch hold used to raise the pose too. That outlived hold-to-talk: with
  // nothing left to dictate into, a steady press — or a body-touch zone latched
  // on by capacitive drift after power-on, which needs no finger at all — sat
  // the robot in "listening..." while the host knew nothing about it. The pose
  // now says exactly what the host said, and nothing else.
  static bool wasListening = false;
  bool listenNow = tama.listening;
  if (listenNow != wasListening) {
    wasListening = listenNow;
    diagLog("listen %s", listenNow ? "on" : "off");
    bodyListen(listenNow);
  }
  chirpSetEnabled(settings().sound);   // mute follows the pet's sound setting
  chirpUpdate();
  bodyLedPolicy(settings().led, listenNow);
  // The host conversation ({"cmd":"agent"}): the robot acts the phase out.
  // The daemon re-sends the current phase every 10 s while a conversation
  // is open; a phase that goes 30 s without one is stale (the daemon died,
  // restarted, or the link dropped mid-conversation — bench 2026-09-06: the
  // head sat in "listening..." for a quarter hour). Fall back to the persona.
  {
    uint8_t ag = tama.agentState;
    // elapsedMs, not `now - agentAtMs`: dataPoll() stamps the phase AFTER `now` was
    // read, and the plain subtraction wrapped to ~4e9 — a phase went "stale" the
    // instant it arrived (three times in one conversation, 2026-09-10). ticks.h.
    if (ag != AG_IDLE && ticks::elapsedMs(now, tama.agentAtMs) > 30000UL) {
      diagLog("agent phase stale -> idle");
      tama.agentState = AG_IDLE; ag = AG_IDLE;
    }
    bodySetAgent((AgentState)ag);
  }
  // The mood engine: what the camera saw, touches, new views and the host's
  // appraisal, integrated every loop; expressed only while exploring.
  {
    static MoodEngine mood;
    static uint32_t moodLastMs = 0;
    static uint32_t moodLogMs = 0;
    uint32_t dt = moodLastMs ? now - moodLastMs : 0;
    moodLastMs = now;
    MoodInput in{};
    in.exploring = tama.explore;
    in.asleep = activeState == P_SLEEP;
    GazeObs ob;
    if (gazeTakeObs(&ob)) { in.motionConf = ob.motionConf; in.faceSeen = ob.faceSeen; in.faceOwner = ob.faceOwner; }
    in.touched = bodyTakeTouched();
    in.newView = bodyTakeNewView();
    if (tama.emoteReq) {
      tama.emoteReq = false;
      in.hostEmote = true; in.dv = tama.emoteDv; in.da = tama.emoteDa;
      diagLog("emote dv=%d da=%d %s", tama.emoteDv, tama.emoteDa, tama.emoteLabel);
    }
    if (tama.snapReq) {
      // The host wants a photo of what eve finds cool: the look task sends
      // the next whole frame at full size; the shutter click is ours.
      tama.snapReq = false;
      look::requestSnap();
      chirpBeep(2600, 25);
      diagLog("snap");
    }
    MoodKind before = mood.kind;
    mood.step(in, dt);
    bool expressing = tama.explore && tama.agentState == AG_IDLE && !listenNow;
    bodySetMood(expressing ? &mood.expr : nullptr);
    moodExprForEyes = expressing ? &mood.expr : nullptr;
    if (mood.kind != before || now - moodLogMs > 60000) {
      moodLogMs = now;
      Serial.printf("[mood] %s v=%.2f a=%.2f social=%.2f stim=%.2f\n", mood.expr.word, mood.v, mood.a,
                    mood.social, mood.stimulation);
    }
  }
  // While a motion runs, report where the head actually is, ten times a second.
  // Nothing else can: the camera frames that normally carry the pose are
  // quarantined for the whole of a movement plus a settling window
  // (look.cpp `quarantined`), which is exactly when the pose is interesting.
  // One short line, ~40 bytes, only while moving.
  {
    static uint32_t lastPoseMs = 0;
    static bool wasMoving = false;
    const bool moving = bodyMotionRunning();
    if (moving && now - lastPoseMs >= 100) {
      lastPoseMs = now;
      Serial.printf("{\"pose\":{\"y\":%d,\"p\":%d}}\n",
                    bodyCmdYawTenths(), bodyCmdPitchTenths());
    }
    if (wasMoving && !moving) {      // one last sample so the end of the bout is recorded
      Serial.printf("{\"pose\":{\"y\":%d,\"p\":%d,\"end\":true}}\n",
                    bodyCmdYawTenths(), bodyCmdPitchTenths());
    }
    wasMoving = moving;
  }

  // A host motion is consumed BEFORE bodyUpdate so it takes the head in the
  // same frame it arrived: the pose it asks for is streamed by this loop's own
  // stepTween, not the next one's. Admission lives on the board (motion.h).
  if (tama.moveReq) {
    const uint8_t req = tama.moveReq;
    tama.moveReq = 0;
    if (req == 3) {
      bodyStopMotion();
      Serial.println("[body] motion stop");
    } else if (hostlook::accepted(false, listenNow, tama.agentState != AG_IDLE, false, true)) {
      uint32_t ms = (req == 1) ? bodyMotion(tama.moveOsc, tama.moveCentreHere)
                               : bodyPlayKeys(tama.moveKeys, tama.moveNKeys);
      Serial.printf("[body] motion %s ran=%ums\n", req == 1 ? "osc" : "keys", (unsigned)ms);
    }
  }
  bodyUpdate(activeState, baseState == P_ATTENTION, now);
  // Camera/memory gaze after the body so its bodyLookAt() lands next frame;
  // eyesLookAt() below reads the head angles every frame, so the eyes follow.
  bodySetExplore(tama.explore);
  {
    GazeHostInput hin;
    hin.camOn = tama.camOn; hin.camFps = tama.camFps; hin.camW = tama.camW; hin.camH = tama.camH;
    hin.wireBusy = xferActive();             // a character transfer owns the wire: skip frames
    hin.faceSeq = tama.faceSeq; hin.faceBx = tama.faceBx; hin.faceBy = tama.faceBy;
    hin.faceSize = tama.faceSize; hin.faceConf = tama.faceConf;
    hin.faceYaw = tama.faceYaw; hin.facePitch = tama.facePitch;
    hin.faceOwner = tama.faceOwner; hin.faceAtMs = tama.faceAtMs;
    hin.hostLookReq = tama.hostLookReq; hin.hostLookYaw = tama.hostLookYaw;
    hin.hostLookPitch = tama.hostLookPitch; hin.hostLookHold = tama.hostLookHold;
    hin.explore = tama.explore; hin.cardUp = false;
    hin.agentActive = tama.agentState != AG_IDLE;
    gazeUpdate(activeState, baseState == P_ATTENTION, listenNow, now, &tama.ownerReset, &hin);
    tama.hostLookReq = hin.hostLookReq;      // consumed by gaze
  }

  diagPhase(DP_GESTURE);
  // touch gestures on the pet: swipe down for Enter, left/right to walk an
  // option picker, tap to raise a waiting terminal or to pat it.
  // Hold-to-talk is gone (owner request): a finger on the pet no longer
  // holds a dictation hotkey, and the board sends no voice frames at all.
  bool gesturesLive = !screenOff;
  if (gesturesLive) {
    if (M5.petSwipedDown()) {
      wake();
      diagLog("swipe down -> enter");
      sendCmd("{\"cmd\":\"key\",\"name\":\"enter\"}");
      beep(2000, 40);
    }
    // Horizontal swipes walk Claude Code's option pickers: right = next
    // option, left = previous. The daemon maps next/prev to Down/Up arrow
    // keys, so swipe-swipe-swipe-down picks an option hands-on-pet.
    if (M5.petSwipedRight()) {
      wake();
      diagLog("swipe right -> next");
      sendCmd("{\"cmd\":\"key\",\"name\":\"next\"}");
      beep(1600, 25);
    }
    if (M5.petSwipedLeft()) {
      wake();
      diagLog("swipe left -> prev");
      sendCmd("{\"cmd\":\"key\",\"name\":\"prev\"}");
      beep(1400, 25);
    }
    // Tap-heart and scrub-dizzy are gone (owner request: touching the pet is
    // functional only — hold to dictate, swipe down for Enter; the pet's
    // moods come from Claude's state, not from being poked). The events are
    // still drained so the compat-layer state machine can't latch.
    // The one functional tap: while the pet demands attention (a session is
    // blocked on the human), tapping it raises that session's terminal.
    // baseState, not activeState: a one-shot overlay (level-up celebrate)
    // must not eat the tap while a session is actually waiting.
    if (M5.petTapped()) {
      wake();
      bool body = M5.petTapWasBody();
      // Two quick taps on the panel also call eve back, but the panel is
      // the unreliable half of this: the Si12T pad on the robot's head
      // registers a pat every time, and reaching for the keyboard to type
      // "stop" at a robot you can touch is not a gesture at all.
      static uint32_t lastTapMs = 0;
      constexpr uint32_t kDoubleTapMs = 450;
      bool doubleTap = lastTapMs && (now - lastTapMs) < kDoubleTapMs;
      lastTapMs = now;
      if (tama.explore && (body || doubleTap)) {
        // Pat it and it comes back: the head stops panning, the host drops
        // explore mode, and it answers with the heart one-shot so the gesture
        // reads as "come here", not as an error.
        lastTapMs = 0;
        diagLog(body ? "pat -> stop exploring" : "double tap -> stop exploring");
        sendCmd("{\"cmd\":\"explore\",\"stop\":true}");
        bodyNoteToucher(0);
        triggerOneShot(P_HEART, 2000);
        beep(1800, 45);
      } else if (baseState == P_ATTENTION) {
        diagLog("tap -> focus");
        sendCmd("{\"cmd\":\"focus\"}");
        beep(1600, 40);
      } else if (body) {
        // A pat on the robot's head (front touch zone) is a real physical
        // gesture, unlike poking the panel: answer it with the heart
        // one-shot (screen art + head tilt + pink LEDs).
        diagLog("head pat -> heart");
        bodyNoteToucher(0);
        triggerOneShot(P_HEART, 2000);
      }
    }
    M5.petScrubbed();
  } else {
    // drain while the screen is off, so a stale gesture can't fire later
    M5.petTapped(); M5.petScrubbed(); M5.petHoldStarted(); M5.petSwipedDown();
    M5.petSwipedLeft(); M5.petSwipedRight();
  }
  // Drained unconditionally: nothing consumes a hold any more, and an
  // unread edge would latch in the compat-layer state machine.
  M5.petHoldStarted(); M5.petHoldEnded();

  // BtnA: step through fake scenarios
  // Button-press wake. BtnB's press is swallowed when it wakes the screen
  // so the same tap doesn't also scroll the transcript. BtnA is wake-only:
  // the screens/menu it used to cycle are gone (owner request — bottom-left
  // does nothing but wake).
  if (M5.BtnA.isPressed() || M5.BtnB.isPressed()) {
    if (screenOff && M5.BtnB.isPressed()) swallowBtnB = true;
    wake();
  }

  // AXP power button (left side): short-press toggles screen off.
  // Long-press (6s) still powers off the device via AXP hardware.
  if (M5.Axp.GetBtnPress() == 0x02) {
    if (screenOff) {
      wake();
    } else {
      M5.Axp.SetLDO2(false);
      screenOff = true;
    }
  }

  // BtnB (bottom-right): scroll back through the transcript HUD.
  if (M5.BtnB.wasPressed()) {
    if (swallowBtnB) { swallowBtnB = false; }
    else {
      beep(2400, 30);
      msgScroll = (msgScroll >= 30) ? 0 : msgScroll + 1;
    }
  }

  // blink bookkeeping

  diagPhase(DP_CLOCK);
  clockRefreshRtc();   // 1Hz internal throttle; also caches _onUsb
  powerFollowUsb();
  // The full-screen clock takeover is retired (owner request: the pet should
  // ALWAYS be visible). Time now lives as a small always-on overlay in the
  // top-left of the normal screen — see drawMiniClock(). The takeover and
  // landscape machinery are kept compiled but permanently gated off.
  bool clocking = false;
  if (clocking) clockUpdateOrient();
  else { clockOrient = 0; orientFrames = 0; paintedOrient = 0; }
  bool landscapeClock = clocking && clockOrient != 0;

  static bool wasClocking = false;
  static bool wasLandscape = false;
  if (clocking != wasClocking || landscapeClock != wasLandscape) {
    if (clocking && !landscapeClock) characterSetPeek(true);
    else { characterSetPeek(false); eveSetPeek(false); repaintAll(); }
    characterInvalidate();
    if (eveMode) eveInvalidate();
    wasClocking = clocking;
    wasLandscape = landscapeClock;
  }
  if (clocking) {
    uint8_t dow = clockDow();
    bool weekend = (dow == 0 || dow == 6);
    bool friday  = (dow == 5);

    uint8_t h = _clkTm.Hours;
    if (h >= 1 && h < 7)             activeState = P_SLEEP;
    else if (weekend)                activeState = (now/8000 % 6 == 0) ? P_HEART : P_SLEEP;
    else if (h < 9)                  activeState = (now/6000 % 4 == 0) ? P_IDLE  : P_SLEEP;
    else if (h == 12)                activeState = (now/5000 % 3 == 0) ? P_HEART : P_IDLE;
    else if (friday && h >= 15)      activeState = (now/4000 % 3 == 0) ? P_CELEBRATE : P_IDLE;
    else if (h >= 22 || h == 0)      activeState = (now/7000 % 3 == 0) ? P_DIZZY : P_SLEEP;
    else                             activeState = (now/10000 % 5 == 0) ? P_SLEEP : P_IDLE;
  }

  static uint32_t lastPasskey = 0;
  uint32_t pk = blePasskey();
  if (pk && !lastPasskey) { wake(); beep(1800, 60); }
  lastPasskey = pk;

  // RENDER covers every draw path — the GIF branch used to carry no phase at
  // all, so a hang in characterTick would have blamed "clock face" (the last
  // phase set before this chain).
  diagPhase(DP_RENDER);
  // The face on this board is RoboEyes (eyes.cpp): a 1-bit canvas drawn into
  // the top EYES_H rows of the sprite every frame, plus one status word under
  // it. The 18 ASCII species and the GIF renderer still compile but are not
  // drawn (eveMode/characterLoaded are ignored here); the sprite/HUD/card
  // pipeline below is the pet build's, unchanged.
  bool expressionAllowed = expression::allowed(napping || screenOff || clocking,
      baseState == P_ATTENTION || activeState == P_ATTENTION, listenNow || tama.agentState == AG_LISTENING,
      tama.agentState == AG_ASKING, tama.agentState == AG_ERROR);
  bool expressionApplied = false, expressionWinked = false;
  if (napping || screenOff || landscapeClock) {
    // skip sprite render — face-down, powered off, or landscape clock
  } else {
    const Palette& p = characterPalette();
    // Caption: one page of eve's reply, wrapped and paced by the host (bridge caption_pager.py).
    // Shown until the next page / clear, or hold_ms + grace if the host goes quiet. Listening eyes
    // on the N row end at y 110, the band starts at 112. A permission card (y >= 126) wins.
    const int CAP_Y = 112, CAP_LINES = 4, CAP_COLS = 17, CAP_LH = 23, CAP_SIZE = 3, CAP_X = 7;
    const uint32_t CAP_GRACE_MS = 3000;
    static_assert(CAP_Y + CAP_LINES * CAP_LH <= EYES_H, "caption band must stay above the HUD");
    static_assert(CAP_X + CAP_COLS * 6 * CAP_SIZE <= W - 6, "caption text must leave the more-tick margin");
    static_assert(CAP_LINES <= TamaState::CAP_MAX_LINES && CAP_COLS <= TamaState::CAP_MAX_COLS, "caption storage");
    bool captionUp = tama.captionAtMs && tama.captionNLines
                     && ticks::elapsedMs(now, tama.captionAtMs) < (uint32_t)tama.captionHoldMs + CAP_GRACE_MS;
    // A pet showing words is awake (face.h): the face never sleeps under a caption
    // or in the gap between pages. The body keeps activeState, so no sleepy chirp.
    static uint32_t lastTextMs = 0;
    if (captionUp) lastTextMs = now ? now : 1;
    PersonaState faceNow = face::faceState(activeState, captionUp, now, lastTextMs);
    eyesSet(faceNow, baseState == P_ATTENTION, listenNow, false, bodyGazeSide(), tama.explore,
            tama.agentState, moodExprForEyes,
            expressionAllowed && semanticExpression.active(now) ? semanticExpression.request.kind : expression::None);
    // A caption page (y >= 112) owns the lower band: the eyes park on the N row.
    eyesCardUp(captionUp);
    // The eyes lead the head toward a face the host just reported (gaze.h): they glance at once, the neck
    // follows about a second later and the lead falls back to zero as the face comes to the centre.
    {
      int ey = bodyYawDeg() + gazeEyeLeadYawDeg();
      int ep = bodyPitchDeg() + gazeEyeLeadPitchDeg();
      eyesLookAt((int8_t)(ey < -120 ? -120 : ey > 120 ? 120 : ey), (int8_t)(ep < 0 ? 0 : ep > 90 ? 90 : ep));
    }
    static uint32_t winkedId = 0;
    if (expressionAllowed && semanticExpression.active(now) && semanticExpression.request.kind == expression::Wink
        && semanticExpression.request.id != winkedId) {
      eyesWink();
      winkedId = semanticExpression.request.id;
      expressionWinked = true;
    }
    eyesTick(now);
    expressionApplied = expressionAllowed && semanticExpression.active(now);
    static uint32_t captionSeenAt = 0;
    if (captionUp && tama.captionAtMs != captionSeenAt) {
      captionSeenAt = tama.captionAtMs;
      if (tama.captionChirp) chirpPlay(CHIRP_TALK);            // one babble per page, host decides
      diagLog("caption p%u/%u %u lines hold %u", (unsigned)tama.captionPage, (unsigned)tama.captionOf,
              (unsigned)tama.captionNLines, (unsigned)tama.captionHoldMs);
    }
    if (!captionUp) captionSeenAt = 0;
    if (captionUp) {
      spr.fillRect(0, CAP_Y - 2, W, EYES_H - CAP_Y + 2, p.bg);
      spr.setTextDatum(TL_DATUM);
      spr.setTextSize(CAP_SIZE);
      spr.setTextColor(eyesColor(), p.bg);
      for (int i = 0; i < tama.captionNLines && i < CAP_LINES; i++)
        spr.drawString(tama.captionLines[i], CAP_X, CAP_Y + i * CAP_LH);
      if (tama.captionOf == 0 || tama.captionPage + 1 < tama.captionOf)
        spr.fillRect(W - 6, EYES_H - 8, 5, 5, eyesColor());   // "more follows" tick, in the free right margin
      spr.setTextSize(1);
    }
    // Status word: cleared and redrawn each frame (the card band overwrites
    // it during a prompt, which is intended — the card is the status then).
    if (!captionUp) spr.fillRect(0, EYES_STATUS_Y, W, 18, p.bg);
    const char* st = captionUp ? "" : eyesStatusText(faceNow, listenNow, tama.explore, tama.agentState, moodExprForEyes);
    if (st[0]) {
      spr.setTextDatum(MC_DATUM);
      spr.setTextSize(2);
      spr.setTextColor(eyesColor(), p.bg);   // same colour as the eyes
      spr.drawString(st, CX, EYES_STATUS_Y + 8);
      spr.setTextDatum(TL_DATUM);
      spr.setTextSize(1);
    }
    if (xferActive()) {
      // Character-pack install progress (GIF packs are not drawn on this
      // board, but the transfer still works and should be visible).
      uint32_t done = xferProgress(), total = xferTotal();
      spr.fillRect(0, EYES_STATUS_Y + 20, W, 20, p.bg);
      spr.setTextColor(p.textDim, p.bg);
      spr.setCursor(8, EYES_STATUS_Y + 22);
      spr.printf("installing %luK / %luK", done/1024, total/1024);
    }
  }
  // Eye-only ACK after the render path ran. Laya never changes sound.
  static uint32_t expressionReportedId = 0;
  static bool expressionReportedActive = false, expressionReportedApplied = false, reportedWinkClosed = false;
  bool winkClosed = expressionApplied && eyesWinkClosed();
  bool expressionActive = semanticExpression.active(now);
  if (semanticExpression.request.id && (expressionReportedId != semanticExpression.request.id
      || expressionReportedActive != expressionActive || expressionReportedApplied != expressionApplied || expressionWinked || reportedWinkClosed != winkClosed)) {
    Serial.printf("{\"expression\":{\"id\":%lu,\"label\":\"%s\",\"active\":%s,\"applied\":%s,\"chirp\":%s,\"wink\":%s,\"wink_closed\":%s,\"muted\":%s,\"phase\":%u,\"fw\":\"%s\"}}\n",
      (unsigned long)semanticExpression.request.id, expression::name(semanticExpression.request.kind),
      expressionActive ? "true" : "false", expressionApplied ? "true" : "false", "false", expressionWinked ? "true" : "false", winkClosed ? "true" : "false",
      settings().sound ? "false" : "true", (unsigned)tama.agentState, CLAUDE_PET_GIT_SHA);
    reportedWinkClosed = winkClosed;
    expressionReportedId = semanticExpression.request.id;
    expressionReportedActive = expressionActive; expressionReportedApplied = expressionApplied;
  }
  if (landscapeClock) {
    drawClock();
  } else if (!napping && !screenOff) {
    if (blePasskey()) drawPasskey();
    else if (clocking) drawClock();
    else if (settings().hud) drawHUD();
    // No clock on the robot's face (owner request). drawMiniClock() stays
    // compiled for the pet build's layout; the RTC sync in data.h is kept.
    // Split phase: every TG0WDT reset of 2026-08-05/06 died in "render",
    // which conflated the draw code above with this full-frame SPI write.
    // The next hang names the guilty half.
    diagPhase(DP_PUSH);
    spr.pushSprite(0, 0);
  }

  // Face-down nap: dim immediately, pause animations, accumulate sleep time.
  // Skipped during approval — you're holding it to read, not sleeping it.
  // Exit needs sustained not-down so IMU noise at the threshold doesn't
  // bounce brightness between 8 and full every few frames.
  static int8_t faceDownFrames = 0;
  diagPhase(DP_NAP);
  {
    bool down = isFaceDown();
    if (down)       { if (faceDownFrames < 20) faceDownFrames++; }
    else            { if (faceDownFrames > -10) faceDownFrames--; }
  }

  if (!napping && faceDownFrames >= 15) {
    napping = true;
    napStartMs = now;
    M5.Axp.ScreenBreath(8);
    dimmed = true;
  } else if (napping && faceDownFrames <= -8) {
    napping = false;
    statsOnNapEnd((now - napStartMs) / 1000);
    statsOnWake();
    wake();
  }

  // millis() not the cached `now`: wake() runs after `now` is captured,
  // so now - lastInteractMs underflows when a button is held → flicker.
  // No auto-off on USB power — clock face wants to stay visible while charging.
  if (!screenOff && !_onUsb
      && millis() - lastInteractMs > SCREEN_OFF_MS) {
    M5.Axp.SetLDO2(false);
    screenOff = true;
  }

  halNoiseTestUpdate();
  diagPhase(DP_IDLE);   // reached the end cleanly — a hang here is the delay
  delay(screenOff ? 100 : 16);
}
