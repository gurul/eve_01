// Claude status display for the Elecrow CrowPanel ESP32 4.2" E-Paper HMI
// (400×300 SSD1683, rendered portrait 300×400).
//
// Same wire protocol as the FNK0104B pet firmware — the eve-bridge
// daemon drives both boards unmodified: NDJSON heartbeats in over UART0 (the
// CH340 USB-C port), `{"cmd":...}` verbs out. This build is purely
// functional — no pet, no animation: a clock, a state banner, session/token
// counters, the tail of the live transcript, and permission prompts as a
// full-screen card decided with the front buttons:
//
//   card showing:    OK short = approve once · OK hold = approve always
//                    (destructive prompts require the hold — a short tap
//                    draws a "hold to approve" hint) · EXIT = deny ·
//                    MENU = raise the asking session's terminal
//   otherwise:       MENU = raise the blocked session's terminal ·
//                    OK = Enter on the Mac · rotary up/down = walk Claude
//                    Code's option pickers
//
// Refresh policy: the framebuffer is cleared and redrawn from scratch on
// every render (no incremental regions), pushed as a partial refresh for
// routine updates and a fast full refresh on boot, on card in/out, and every
// FULL_EVERY_PARTIALS partials to deghost. The vendored driver keeps the
// SSD1683's old-data plane in sync (see EPD.cpp) so partials never overlap
// stale pixels. Panel deep-sleeps between updates. Redraws trigger on state
// changes, prompt traffic, and the minute tick — transcript/token updates
// ride along on the next tick rather than triggering their own.

#include <Arduino.h>
#include <ArduinoJson.h>
#include <Preferences.h>
#include <esp_system.h>
#include "EPD.h"
#include "EPD_GUI.h"

// ---------------------------------------------------------------- pins ----
#define PIN_EPD_POWER 7   // HIGH = panel powered (vendor demo does this)
#define PIN_BTN_MENU  2   // "HOME" on the silkscreen
#define PIN_BTN_EXIT  1
#define PIN_BTN_NEXT  4   // rotary down
#define PIN_BTN_OK    5   // rotary press
#define PIN_BTN_PREV  6   // rotary up

// ------------------------------------------------------------- display ----
// Portrait: the dock stands the panel on its short edge, USB-C down. If the
// image comes up upside down in your dock, flip this to 270.
#define UI_ROTATE 90
#define UI_W 300
#define UI_H 400

static uint8_t fb[EPD_W / 8 * EPD_H];   // 15000 bytes, 1bpp (native landscape)

const uint32_t FULL_EVERY_PARTIALS = 24;
const uint32_t HOLD_MS             = 700;    // OK long-press threshold

// --------------------------------------------------------------- state ----
enum PersonaState { P_SLEEP, P_IDLE, P_BUSY, P_ATTENTION, P_CELEBRATE, P_DIZZY, P_HEART };
static const char* stateNames[] = { "sleep", "idle", "busy", "attention", "celebrate", "dizzy", "heart" };

struct TamaState {
  uint8_t  sessionsTotal = 0, sessionsRunning = 0, sessionsWaiting = 0;
  bool     recentlyCompleted = false;
  uint32_t tokensToday = 0;
  bool     connected = false;
  char     msg[24] = "";
  char     lines[8][92];
  uint8_t  nLines = 0;
  char     promptId[40] = "", promptTool[20] = "", promptHint[44] = "", promptSess[20] = "";
  uint8_t  promptQueued = 0;
  uint16_t promptTtl = 0, promptTtlMax = 0;
  uint32_t promptTtlAtMs = 0;
  bool     promptHot = false;
  char     promptDetail[724] = "";
};

static TamaState tama;
static uint32_t  lastLiveMs = 0;
static Preferences prefs;
static char petNameBuf[24] = "Claude";
static char ownerBuf[24]   = "";
static uint32_t approvals = 0, denials = 0;

// Clock: bridge sends {"time":[epoch,tz_offset]}; we keep the local epoch at
// sync plus the millis() it arrived and derive from there. No RTC involved.
static bool     timeValid = false;
static time_t   localEpochAtSync = 0;
static uint32_t syncMs = 0;

static time_t nowLocal() {
  return localEpochAtSync + (time_t)((millis() - syncMs) / 1000);
}

// ------------------------------------------------------------- serial -----
static void sendLine(const char* json) { Serial.println(json); }

static void sendPermission(const char* decision) {
  char b[96];
  snprintf(b, sizeof(b), "{\"cmd\":\"permission\",\"id\":\"%s\",\"decision\":\"%s\"}",
           tama.promptId, decision);
  sendLine(b);
}

static void sendFocus(const char* id) {
  char b[80];
  if (id && id[0]) snprintf(b, sizeof(b), "{\"cmd\":\"focus\",\"id\":\"%s\"}", id);
  else             snprintf(b, sizeof(b), "{\"cmd\":\"focus\"}");
  sendLine(b);
}

static void sendKey(const char* name) {
  char b[48];
  snprintf(b, sizeof(b), "{\"cmd\":\"key\",\"name\":\"%s\"}", name);
  sendLine(b);
}

static void sendVoice(const char* state) {
  char b[48];
  snprintf(b, sizeof(b), "{\"cmd\":\"voice\",\"state\":\"%s\"}", state);
  sendLine(b);
}

static void sendAck(const char* what, bool ok, const char* error = nullptr) {
  char b[128];
  if (error)
    snprintf(b, sizeof(b), "{\"ack\":\"%s\",\"ok\":false,\"n\":0,\"error\":\"%s\"}", what, error);
  else
    snprintf(b, sizeof(b), "{\"ack\":\"%s\",\"ok\":%s,\"n\":0}", what, ok ? "true" : "false");
  sendLine(b);
}

static void sendStatusAck() {
  // Same shape as the touch build's status ack — the daemon's ack watchdog
  // and `eve-bridge status` both read it. No battery gauge is wired
  // here (the BAT socket has no sense line in the vendor pinout), so report
  // USB-powered and full.
  char b[320];
  snprintf(b, sizeof(b),
    "{\"ack\":\"status\",\"ok\":true,\"n\":0,\"data\":{"
    "\"name\":\"%s\",\"owner\":\"%s\",\"sec\":false,"
    "\"bat\":{\"pct\":100,\"mV\":5000,\"mA\":0,\"usb\":true},"
    "\"sys\":{\"up\":%lu,\"heap\":%u,\"fsFree\":0,\"fsTotal\":0},"
    "\"stats\":{\"appr\":%lu,\"deny\":%lu,\"vel\":0,\"nap\":0,\"lvl\":1}"
    "}}",
    petNameBuf, ownerBuf, millis() / 1000, ESP.getFreeHeap(),
    (unsigned long)approvals, (unsigned long)denials);
  sendLine(b);
}

static void sendDiag() {
  static const char* const RESETS[] = {
    "unknown", "poweron", "external", "software", "panic", "int-wdt",
    "task-wdt", "other-wdt", "deepsleep", "brownout", "sdio" };
  int r = (int)esp_reset_reason();
  if (r < 0 || r > 10) r = 0;
  char b[160];
  snprintf(b, sizeof(b),
    "{\"diag\":{\"boot\":0,\"reset\":\"%s\",\"up\":%lu,\"heap\":%u,"
    "\"minheap\":%u,\"psram\":0}}",
    RESETS[r], millis() / 1000, ESP.getFreeHeap(), ESP.getMinFreeHeap());
  sendLine(b);
}

// ----------------------------------------------------- inbound commands ---
static bool handleCommand(JsonDocument& doc) {
  const char* cmd = doc["cmd"];
  if (!cmd) return false;

  if (strcmp(cmd, "status") == 0)  { sendStatusAck(); return true; }
  if (strcmp(cmd, "diag") == 0)    { sendDiag(); return true; }
  if (strcmp(cmd, "unpair") == 0)  { sendAck("unpair", true); return true; }
  if (strcmp(cmd, "species") == 0) { sendAck("species", true); return true; }  // no pet on this board
  if (strcmp(cmd, "name") == 0) {
    const char* n = doc["name"];
    if (n) { strlcpy(petNameBuf, n, sizeof(petNameBuf)); prefs.putString("name", petNameBuf); }
    sendAck("name", n != nullptr);
    return true;
  }
  if (strcmp(cmd, "owner") == 0) {
    const char* n = doc["name"];
    if (n) { strlcpy(ownerBuf, n, sizeof(ownerBuf)); prefs.putString("owner", ownerBuf); }
    sendAck("owner", n != nullptr);
    return true;
  }
  // GIF character push: refuse at the handshake so the host never streams
  // hundreds of KB at a 1-bit panel with no filesystem for it.
  if (strcmp(cmd, "char_begin") == 0) { sendAck("char_begin", false, "eink board: no character storage"); return true; }
  if (strcmp(cmd, "file") == 0 || strcmp(cmd, "chunk") == 0 ||
      strcmp(cmd, "file_end") == 0 || strcmp(cmd, "char_end") == 0) {
    sendAck(cmd, false);
    return true;
  }
  return strcmp(cmd, "permission") != 0;   // permission is board→host, not ours
}

static void applyJson(const char* line) {
  JsonDocument doc;
  if (deserializeJson(doc, line)) return;
  if (handleCommand(doc)) { lastLiveMs = millis(); return; }

  JsonArray t = doc["time"];
  if (!t.isNull() && t.size() == 2) {
    localEpochAtSync = (time_t)t[0].as<uint32_t>() + (int32_t)t[1];
    syncMs = millis();
    timeValid = true;
    lastLiveMs = millis();
    return;
  }

  tama.sessionsTotal     = doc["total"]     | tama.sessionsTotal;
  tama.sessionsRunning   = doc["running"]   | tama.sessionsRunning;
  tama.sessionsWaiting   = doc["waiting"]   | tama.sessionsWaiting;
  tama.recentlyCompleted = doc["completed"] | false;
  tama.tokensToday       = doc["tokens_today"] | tama.tokensToday;
  const char* m = doc["msg"];
  if (m) strlcpy(tama.msg, m, sizeof(tama.msg));

  JsonArray la = doc["entries"];
  if (!la.isNull()) {
    uint8_t n = 0;
    for (JsonVariant v : la) {
      if (n >= 8) break;
      const char* s = v.as<const char*>();
      strlcpy(tama.lines[n], s ? s : "", sizeof(tama.lines[0]));
      n++;
    }
    tama.nLines = n;
  }

  JsonObject pr = doc["prompt"];
  if (!pr.isNull()) {
    const char* pid = pr["id"];
    bool fresh = strcmp(tama.promptId, pid ? pid : "") != 0;
    strlcpy(tama.promptId,   pid       ? pid                          : "", sizeof(tama.promptId));
    strlcpy(tama.promptTool, pr["tool"] ? pr["tool"].as<const char*>() : "", sizeof(tama.promptTool));
    strlcpy(tama.promptHint, pr["hint"] ? pr["hint"].as<const char*>() : "", sizeof(tama.promptHint));
    strlcpy(tama.promptSess, pr["sess"] ? pr["sess"].as<const char*>() : "", sizeof(tama.promptSess));
    tama.promptQueued = pr["queued"] | 0;
    uint16_t ttl = pr["ttl"] | 0;
    tama.promptTtl = ttl;
    tama.promptTtlAtMs = millis();
    if (fresh || ttl > tama.promptTtlMax) tama.promptTtlMax = ttl;
    const char* risk = pr["risk"];
    tama.promptHot = risk && strcmp(risk, "hot") == 0;
    const char* pd = pr["detail"];
    strlcpy(tama.promptDetail, pd ? pd : "", sizeof(tama.promptDetail));
  } else {
    tama.promptId[0] = 0; tama.promptTool[0] = 0; tama.promptHint[0] = 0;
    tama.promptSess[0] = 0; tama.promptDetail[0] = 0;
    tama.promptQueued = 0; tama.promptTtl = 0; tama.promptTtlMax = 0;
    tama.promptHot = false;
  }
  lastLiveMs = millis();
}

// Sized so a heartbeat carrying the full 720-byte prompt detail still fits
// on one line (same reasoning as the touch build's data.h).
static char   lineBuf[2048];
static size_t lineLen = 0;

static void pollSerial() {
  while (Serial.available()) {
    char c = Serial.read();
    if (c == '\n' || c == '\r') {
      if (lineLen > 0) {
        lineBuf[lineLen] = 0;
        if (lineBuf[0] == '{') applyJson(lineBuf);
        lineLen = 0;
      }
    } else if (lineLen < sizeof(lineBuf) - 1) {
      lineBuf[lineLen++] = c;
    }
  }
}

static bool dataConnected() {
  return lastLiveMs != 0 && (millis() - lastLiveMs) <= 30000;
}

static PersonaState derive() {
  if (!tama.connected)          return P_IDLE;
  if (tama.sessionsWaiting > 0) return P_ATTENTION;
  if (tama.recentlyCompleted)   return P_CELEBRATE;
  if (tama.sessionsRunning >= 3) return P_BUSY;
  return P_IDLE;
}

// ------------------------------------------------------------ rendering ---
// font metrics: advance = size/2 px (8→6). All fonts are monospace.
static uint16_t textW(const char* s, uint8_t size) {
  return strlen(s) * (size == 8 ? 6 : size / 2);
}
static void centered(uint16_t y, const char* s, uint8_t size) {
  uint16_t w = textW(s, size);
  EPD_ShowString(w >= UI_W ? 0 : (UI_W - w) / 2, y, s, size, BLACK);
}

// Wrap `s` into lines of `cols` chars at word boundaries where possible,
// drawing up to maxLines at (x, y0) with lineH spacing. Returns lines drawn.
static uint8_t wrapText(const char* s, uint16_t x, uint16_t y0, uint8_t cols,
                        uint8_t maxLines, uint8_t lineH, uint8_t font) {
  char line[80];
  uint8_t drawn = 0;
  const char* p = s;
  while (*p && drawn < maxLines) {
    size_t n = strlen(p);
    size_t take = n <= cols ? n : cols;
    if (n > cols) {
      // back up to the last space inside the window, if there is one
      for (size_t i = take; i > cols / 2; i--) {
        if (p[i] == ' ') { take = i; break; }
      }
    }
    if (take >= sizeof(line)) take = sizeof(line) - 1;
    memcpy(line, p, take); line[take] = 0;
    EPD_ShowString(x, y0 + drawn * lineH, line, font, BLACK);
    p += take;
    while (*p == ' ') p++;
    drawn++;
  }
  return drawn;
}

static void fmtTokens(char* out, size_t n, uint32_t tok) {
  if (tok >= 1000)
    snprintf(out, n, "%lu.%luk tok", (unsigned long)(tok / 1000),
             (unsigned long)(tok % 1000) / 100);
  else
    snprintf(out, n, "%lu tok", (unsigned long)tok);
}

static uint32_t partialsSinceFull = 0;
static bool     needFull = true;      // first draw after boot is a full one
static bool     holdHint = false;     // hot prompt: short OK tap draws this

static void pushFrame() {
  if (needFull || partialsSinceFull >= FULL_EVERY_PARTIALS) {
    EPD_Init_Fast(Fast_Seconds_1_5s);
    EPD_Display_Fast(fb);
    partialsSinceFull = 0;
    needFull = false;
  } else {
    // Wake from deep sleep the way GxEPD2 does on this panel (reset + soft
    // reset + temp sensor select) — NOT the full EPD_Init(). The controller
    // retains both RAM planes through a properly-entered deep sleep, so the
    // partial can diff against the previous frame.
    EPD_Wake();
    EPD_Display_Part(0, 0, EPD_W, EPD_H, fb);
    partialsSinceFull++;
  }
  EPD_Sleep();
}

static const char* banner(PersonaState st) {
  if (!tama.connected) return "NO CLAUDE";
  switch (st) {
    case P_ATTENTION: return "NEEDS YOU";
    case P_CELEBRATE: return "DONE!";
    case P_BUSY:      return "WORKING";
    default:          return tama.sessionsRunning > 0 ? "WORKING" : "IDLE";
  }
}

static void drawStatusScreen(PersonaState st) {
  Paint_NewImage(fb, EPD_W, EPD_H, UI_ROTATE, WHITE);
  EPD_Full(WHITE);

  char b[64];
  if (timeValid) {
    struct tm lt; time_t n = nowLocal(); gmtime_r(&n, &lt);
    snprintf(b, sizeof(b), "%2d:%02d", lt.tm_hour, lt.tm_min);
    EPD_ShowString((UI_W - 5 * 24) / 2, 8, b, 48, BLACK);
    static const char* const WD[] = { "Sun","Mon","Tue","Wed","Thu","Fri","Sat" };
    static const char* const MO[] = { "Jan","Feb","Mar","Apr","May","Jun",
                                      "Jul","Aug","Sep","Oct","Nov","Dec" };
    snprintf(b, sizeof(b), "%s %d %s", WD[lt.tm_wday], lt.tm_mday, MO[lt.tm_mon]);
    centered(62, b, 16);
  } else {
    EPD_ShowString((UI_W - 5 * 24) / 2, 8, "--:--", 48, BLACK);
  }

  EPD_DrawLine(0, 86, UI_W - 1, 86, BLACK);

  centered(98, banner(st), 24);

  snprintf(b, sizeof(b), "%u sessions  %u running", tama.sessionsTotal, tama.sessionsRunning);
  centered(130, b, 16);
  char tok[24]; fmtTokens(tok, sizeof(tok), tama.tokensToday);
  snprintf(b, sizeof(b), "%u waiting  %s", tama.sessionsWaiting, tok);
  centered(150, b, 16);
  if (tama.msg[0]) centered(172, tama.msg, 16);

  EPD_DrawLine(0, 194, UI_W - 1, 194, BLACK);

  // transcript tail: newest entries last, wrapped to two rows each, clipped
  // to the space above the legend
  uint16_t y = 202;
  const uint16_t yMax = 368;
  for (uint8_t i = 0; i < tama.nLines; i++) {
    if (y + 14 > yMax) break;
    uint8_t rows = wrapText(tama.lines[i], 4, y, 48, (yMax - y) / 14 >= 2 ? 2 : 1, 14, 12);
    y += rows * 14 + 4;
  }

  EPD_DrawLine(0, 372, UI_W - 1, 372, BLACK);
  centered(382, "OK enter  <> pick  EXIT talk  MENU focus", 12);
}

static void drawPromptScreen() {
  Paint_NewImage(fb, EPD_W, EPD_H, UI_ROTATE, WHITE);
  EPD_Full(WHITE);

  EPD_DrawRectangle(0, 0, UI_W - 1, UI_H - 1, BLACK, 0);
  if (tama.promptHot) {
    EPD_DrawRectangle(2, 2, UI_W - 3, UI_H - 3, BLACK, 0);
    EPD_DrawRectangle(4, 4, UI_W - 5, UI_H - 5, BLACK, 0);
  }

  centered(14, tama.promptTool[0] ? tama.promptTool : "permission", 24);
  char b[64];
  if (tama.promptQueued > 0)
    snprintf(b, sizeof(b), "%s  +%u queued", tama.promptSess, tama.promptQueued);
  else
    snprintf(b, sizeof(b), "%s", tama.promptSess);
  centered(44, b, 16);

  uint8_t hintLines = wrapText(tama.promptHint, 10, 70, 35, 2, 20, 16);
  uint16_t detailY = 70 + hintLines * 20 + 8;
  wrapText(tama.promptDetail[0] ? tama.promptDetail : "", 10, detailY, 47,
           (uint8_t)((330 - detailY) / 14), 14, 12);

  // ttl drain bar, quantized in the render trigger so it redraws ~5×/prompt
  if (tama.promptTtlMax > 0) {
    int32_t left = (int32_t)tama.promptTtl - (int32_t)((millis() - tama.promptTtlAtMs) / 1000);
    if (left < 0) left = 0;
    uint16_t w = (uint16_t)((uint32_t)(UI_W - 20) * left / tama.promptTtlMax);
    EPD_DrawRectangle(10, 340, UI_W - 10, 346, BLACK, 0);
    if (w > 0) EPD_DrawRectangle(10, 340, 10 + w, 346, BLACK, 1);
  }

  if (holdHint) {
    centered(358, "destructive command:", 16);
    centered(378, "HOLD OK to approve", 16);
  } else if (tama.promptHot) {
    centered(358, "HOLD OK approve   < deny", 16);
    centered(380, "MENU show terminal", 12);
  } else {
    centered(358, "> approve   < deny", 16);
    centered(380, "hold OK = always   MENU show terminal", 12);
  }
}

// ------------------------------------------------- render change tracking -
// One signature integer per "thing worth a refresh"; render when it moves.
static uint32_t drawnSig = 0xFFFFFFFF;
static PersonaState lastBase = P_SLEEP;
static uint32_t celebrateUntil = 0;

static uint8_t ttlBucket() {
  if (!tama.promptId[0] || tama.promptTtlMax == 0) return 0;
  int32_t left = (int32_t)tama.promptTtl - (int32_t)((millis() - tama.promptTtlAtMs) / 1000);
  if (left < 0) left = 0;
  return (uint8_t)((uint32_t)left * 5 / tama.promptTtlMax);
}

static uint32_t promptSigHash() {
  // cheap FNV over the prompt id — the id changing is what matters
  uint32_t h = 2166136261u;
  for (const char* p = tama.promptId; *p; p++) { h ^= (uint8_t)*p; h *= 16777619u; }
  return h;
}

void setup() {
  Serial.setRxBufferSize(4096);   // a 2KB heartbeat must survive a ~1s render
  Serial.begin(115200);

  prefs.begin("eink_pet", false);
  prefs.getString("name", petNameBuf, sizeof(petNameBuf));
  prefs.getString("owner", ownerBuf, sizeof(ownerBuf));
  approvals = prefs.getULong("appr", 0);
  denials   = prefs.getULong("deny", 0);

  pinMode(PIN_BTN_MENU, INPUT);   // external pull-ups on the board
  pinMode(PIN_BTN_EXIT, INPUT);
  pinMode(PIN_BTN_NEXT, INPUT);
  pinMode(PIN_BTN_OK,   INPUT);
  pinMode(PIN_BTN_PREV, INPUT);

  pinMode(PIN_EPD_POWER, OUTPUT);
  digitalWrite(PIN_EPD_POWER, HIGH);
  EPD_GPIOInit();
  EPD_Clear();          // one true full clear at boot; also inits the panel

  Serial.println("[boot] claude_pet_eink up");
}

// ------------------------------------------------------------- buttons ----
struct Btn {
  uint8_t pin;
  bool down = false;
  uint32_t edgeAt = 0, downAt = 0;
};
static Btn btns[5] = { {PIN_BTN_MENU}, {PIN_BTN_EXIT}, {PIN_BTN_NEXT}, {PIN_BTN_OK}, {PIN_BTN_PREV} };
enum { B_MENU, B_EXIT, B_NEXT, B_OK, B_PREV };

// Push-to-talk: EXIT held with no card up = the daemon holds your dictation
// hotkey until release, exactly like holding the pet on the touch build (the
// Mac's mic does the listening — the board only asks for the hold). The
// prompt-ness is latched at press time so a card arriving mid-dictation
// can't turn the release into a surprise deny — the release just ends the
// hold, and the card waits for its own press.
static bool voiceHeld = false;

static void onButtonDown(uint8_t which) {
  if (which == B_EXIT && tama.promptId[0] == 0) {
    voiceHeld = true;
    sendVoice("start");
  }
}

static void onButton(uint8_t which, uint32_t heldMs) {
  bool prompt = tama.promptId[0] != 0;
  switch (which) {
    case B_OK:
      if (prompt) {
        if (tama.promptHot && heldMs < HOLD_MS) { holdHint = true; drawnSig = 0xFFFFFFFF; break; }
        approvals++; prefs.putULong("appr", approvals);
        sendPermission(!tama.promptHot && heldMs >= HOLD_MS ? "always" : "once");
      } else {
        sendKey("enter");
      }
      break;
    case B_EXIT:
      if (voiceHeld) { voiceHeld = false; sendVoice("stop"); break; }
      if (prompt) { denials++; prefs.putULong("deny", denials); sendPermission("deny"); }
      break;
    case B_MENU:
      sendFocus(prompt ? tama.promptId : nullptr);
      break;
    // With a card up the slider decides, like the touch build's swipe:
    // flick right = approve, left = deny. In the dock the rocker's NEXT
    // (GPIO4) direction is physical RIGHT — verified 2026-08-11 when the
    // opposite mapping turned an intended approve into two PR denials. A
    // flick can't "hold", so destructive prompts route the approve through
    // the HOLD-OK gate.
    case B_NEXT:
      if (prompt) {
        if (tama.promptHot) { holdHint = true; drawnSig = 0xFFFFFFFF; break; }
        approvals++; prefs.putULong("appr", approvals);
        sendPermission("once");
      } else sendKey("next");
      break;
    case B_PREV:
      if (prompt) { denials++; prefs.putULong("deny", denials); sendPermission("deny"); }
      else sendKey("prev");
      break;
  }
}

static void pollButtons() {
  uint32_t now = millis();
  for (uint8_t i = 0; i < 5; i++) {
    bool raw = digitalRead(btns[i].pin) == LOW;
    if (raw != btns[i].down && now - btns[i].edgeAt > 30) {
      btns[i].down = raw;
      btns[i].edgeAt = now;
      if (raw) { btns[i].downAt = now; onButtonDown(i); }
      else     onButton(i, now - btns[i].downAt);
    }
  }
}

// ---------------------------------------------------------------- loop ----
void loop() {
  pollSerial();
  pollButtons();

  uint32_t now = millis();
  tama.connected = dataConnected();

  static uint32_t lastAlive = 0;
  if (now - lastAlive >= 5000) {
    lastAlive = now;
    char b[96];
    snprintf(b, sizeof(b), "[alive] up=%lus heap=%u state=%s",
             now / 1000, ESP.getFreeHeap(), stateNames[lastBase]);
    Serial.println(b);
  }

  // celebrate is a one-shot banner: entering it starts an 8s window,
  // afterwards the base state shows even if `completed` is still set
  PersonaState base = derive();
  if (base == P_CELEBRATE) {
    if (lastBase != P_CELEBRATE) celebrateUntil = now + 8000;
    else if ((int32_t)(now - celebrateUntil) >= 0) base = tama.sessionsRunning >= 3 ? P_BUSY : P_IDLE;
  }
  lastBase = base;

  bool prompt = tama.promptId[0] != 0;
  if (!prompt) holdHint = false;

  uint8_t minute = 0xFF;
  if (timeValid) { struct tm lt; time_t n = nowLocal(); gmtime_r(&n, &lt); minute = lt.tm_min; }

  uint32_t sig = prompt
    ? (0x80000000u | promptSigHash() ^ ((uint32_t)tama.promptQueued << 8)
       ^ ((uint32_t)ttlBucket() << 12) ^ ((uint32_t)holdHint << 16))
    : (((uint32_t)base << 0) | ((uint32_t)tama.connected << 5)
       | ((uint32_t)minute << 6) | ((uint32_t)tama.sessionsTotal << 12)
       | ((uint32_t)tama.sessionsRunning << 18) | ((uint32_t)tama.sessionsWaiting << 24));

  if (sig != drawnSig) {
    static bool wasPrompt = false;
    if (prompt != wasPrompt) needFull = true;   // card in/out is a big inversion — flash it clean
    wasPrompt = prompt;
    if (prompt) drawPromptScreen();
    else        drawStatusScreen(base);
    pushFrame();
    drawnSig = sig;
  }

  delay(10);
}
