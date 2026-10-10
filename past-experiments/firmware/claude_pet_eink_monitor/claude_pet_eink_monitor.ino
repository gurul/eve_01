// Claude agent monitor for the Elecrow CrowPanel ESP32 4.2" E-Paper HMI
// (400×300 SSD1683, rendered landscape 400×300).
//
// This is the *monitor* build: a read-only status panel. It renders a clock,
// a state banner, one row per live Claude Code session, and the tail of the
// transcript. It never draws a permission card and its buttons do nothing —
// approvals happen in the terminal. The daemon must run with
// EVE_MONITOR_ONLY=1 so it takes the defer path instead of blocking a
// tool call for PERMISSION_WAIT_SECS on a button that will never be pressed.
//
// Wire protocol is unchanged from the portrait build — NDJSON heartbeats in
// over UART0 (the CH340 USB-C port). We additionally read the `agents` array
// (see bridge state.py:agent_rows); builds that don't send it just render an
// empty agent list. `cmd:status` / `cmd:diag` acks are still answered, because
// the daemon's ack watchdog and `eve-bridge status` both read them.
//
// Orientation: the panel lies in the stand on its LONG edge, USB-C to the
// LEFT as you look at it. That is the panel's native raster orientation, so
// the canvas is the memory frame 1:1 and UI_ROTATE is 0 (Paint_SetPixel,
// EPD_GUI.cpp). Verified on the bench 2026-08-21 — ROTATE_180 came out upside
// down, so do not "fix" this back by reasoning about the rotation math; the
// two landscape values differ only by which end the USB-C sits at. Stand the
// board on its short edge instead and you want the portrait build's
// ROTATE_90 with UI_W/UI_H swapped back to 300/400.
//
// Refresh policy: full framebuffer redraw AND a full panel init+refresh on
// every frame, rate-limited by MIN_FRAME_MS. This deliberately drops the
// portrait build's partial-refresh path, which wedges this panel — see the
// long note on pushFrame(). Redraws trigger on state changes, the agent list
// changing, and the minute tick; token and transcript updates ride along on
// the next such frame rather than forcing one of their own.

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
// Landscape. See the orientation note in the header comment.
#define UI_ROTATE 0
#define UI_W 400
#define UI_H 300

static uint8_t fb[EPD_W / 8 * EPD_H];   // 15000 bytes, 1bpp (native landscape)


// Agent rows the panel has vertical room for. Must match (or exceed) the
// bridge's State.MAX_AGENT_ROWS — extra rows on the wire are dropped here.
#define MAX_AGENTS 6

// --------------------------------------------------------------- state ----
enum PersonaState { P_SLEEP, P_IDLE, P_BUSY, P_ATTENTION, P_CELEBRATE, P_DIZZY, P_HEART };
static const char* stateNames[] = { "sleep", "idle", "busy", "attention", "celebrate", "dizzy", "heart" };

struct AgentRow {
  char name[16];
  char status[6];   // "wait" | "run" | "idle"
  char tool[14];
};

struct TamaState {
  uint8_t  sessionsTotal = 0, sessionsRunning = 0, sessionsWaiting = 0;
  bool     recentlyCompleted = false;
  uint32_t tokensToday = 0;
  bool     connected = false;
  char     msg[24] = "";
  char     lines[8][92];
  uint8_t  nLines = 0;
  AgentRow agents[MAX_AGENTS];
  uint8_t  nAgents = 0;
};

static TamaState tama;
static uint32_t  lastLiveMs = 0;
static Preferences prefs;
static char petNameBuf[24] = "Claude";
static char ownerBuf[24]   = "";
// Kept only so the status ack keeps reporting the counters the daemon has
// always read. Nothing in this build increments them — the buttons are inert.
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

  // Per-agent rows. Absent key leaves the previous list standing (a build
  // that never sends it simply shows none); present-but-empty clears it,
  // which is what "all sessions ended" looks like on the wire.
  JsonArray ag = doc["agents"];
  if (!ag.isNull()) {
    uint8_t n = 0;
    for (JsonObject o : ag) {
      if (n >= MAX_AGENTS) break;
      strlcpy(tama.agents[n].name,   o["n"] | "", sizeof(tama.agents[n].name));
      strlcpy(tama.agents[n].status, o["s"] | "", sizeof(tama.agents[n].status));
      strlcpy(tama.agents[n].tool,   o["t"] | "", sizeof(tama.agents[n].tool));
      n++;
    }
    tama.nAgents = n;
  }

  lastLiveMs = millis();
}

// Sized to match the portrait build's buffer so an over-long heartbeat from a
// mixed-version daemon still lands on one line.
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
static void rightAligned(uint16_t yRight, uint16_t margin, const char* s, uint8_t size) {
  uint16_t w = textW(s, size);
  EPD_ShowString(w + margin >= UI_W ? 0 : UI_W - margin - w, yRight, s, size, BLACK);
}
static void centered(uint16_t y, const char* s, uint8_t size) {
  uint16_t w = textW(s, size);
  EPD_ShowString(w >= UI_W ? 0 : (UI_W - w) / 2, y, s, size, BLACK);
}

// Draw `s` clipped to `cols` characters at (x, y). Landscape rows are one
// line each — a wrapped agent row would push every row below it out of place.
static void clipped(uint16_t x, uint16_t y, const char* s, uint8_t cols, uint8_t font) {
  char buf[24];
  if (cols >= sizeof(buf)) cols = sizeof(buf) - 1;
  strlcpy(buf, s, (size_t)cols + 1);
  EPD_ShowString(x, y, buf, font, BLACK);
}

static void fmtTokens(char* out, size_t n, uint32_t tok) {
  if (tok >= 1000)
    snprintf(out, n, "%lu.%luk tok", (unsigned long)(tok / 1000),
             (unsigned long)(tok % 1000) / 100);
  else
    snprintf(out, n, "%lu tok", (unsigned long)tok);
}

// Full init + full refresh on EVERY frame — the exact sequence EPD_Clear()
// runs at boot, which is the only one observed to work reliably here.
//
// The portrait build's cheaper path (EPD_Sleep, then EPD_Wake + a partial)
// wedges this panel after a few cycles. EPD_Wake() does reset + soft-reset +
// temperature-select ONLY; it never re-sends data-entry mode (0x11), the
// address window, or the cursor, all of which EPD_Init() sets and all of
// which a hardware reset clears. So from the second wake onward the partial
// writes into an unconfigured controller and nothing lands. It fails SILENTLY
// because EPD_ReadBusy() gives up after 8s and returns normally — which is
// why the render counter kept climbing over a frozen screen.
//
// Re-initialising every frame also makes the panel self-healing: a controller
// wedged by a mid-refresh reset (unplugging USB, a reflash) is recovered by
// the next frame instead of staying dead until someone power-cycles it. Cost
// is a ~2s flashing refresh, which is the right trade for a status board that
// updates on the minute — and it deghosts for free.
static void pushFrame() {
  EPD_Init();
  EPD_Display(fb);
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

// Column origins for an agent row, in canvas px. font 16 advances 8px/char.
#define COL_NAME   6
#define COL_STATE  128
#define COL_TOOL   176

static void drawStatusScreen(PersonaState st) {
  Paint_NewImage(fb, EPD_W, EPD_H, UI_ROTATE, WHITE);
  EPD_Full(WHITE);

  char b[64];

  // ---- header: clock left, banner right, date + tokens beneath ----
  // 12-hour clock: big digits with no leading zero ("3:31"), AM/PM in small
  // type just past them. The marker is placed off the measured digit width
  // rather than a constant so "12:31" — two px-columns wider than "3:31" —
  // can't run into it.
  if (timeValid) {
    struct tm lt; time_t n = nowLocal(); gmtime_r(&n, &lt);
    int h12 = lt.tm_hour % 12;
    if (h12 == 0) h12 = 12;               // midnight and noon are 12, not 0
    snprintf(b, sizeof(b), "%d:%02d", h12, lt.tm_min);
    EPD_ShowString(COL_NAME, 2, b, 48, BLACK);
    uint16_t afterClock = COL_NAME + textW(b, 48) + 8;
    EPD_ShowString(afterClock, 34, lt.tm_hour < 12 ? "AM" : "PM", 16, BLACK);
    static const char* const WD[] = { "Sun","Mon","Tue","Wed","Thu","Fri","Sat" };
    static const char* const MO[] = { "Jan","Feb","Mar","Apr","May","Jun",
                                      "Jul","Aug","Sep","Oct","Nov","Dec" };
    snprintf(b, sizeof(b), "%s %d %s", WD[lt.tm_wday], lt.tm_mday, MO[lt.tm_mon]);
    EPD_ShowString(afterClock + 2 * 8 + 8, 34, b, 16, BLACK);
  } else {
    EPD_ShowString(COL_NAME, 2, "--:--", 48, BLACK);
  }

  rightAligned(4, 6, banner(st), 24);
  char tok[24]; fmtTokens(tok, sizeof(tok), tama.tokensToday);
  rightAligned(34, 6, tok, 16);

  EPD_DrawLine(0, 56, UI_W - 1, 56, BLACK);

  // ---- agent list ----
  // Counts ride on the column-header row: redundant with the rows themselves
  // until there are more sessions than MAX_AGENTS, which is exactly when the
  // list stops telling the whole story.
  EPD_ShowString(COL_NAME,  60, "AGENT",  12, BLACK);
  EPD_ShowString(COL_STATE, 60, "STATE",  12, BLACK);
  EPD_ShowString(COL_TOOL,  60, "TOOL",   12, BLACK);
  snprintf(b, sizeof(b), "%u sess %u run", tama.sessionsTotal, tama.sessionsRunning);
  rightAligned(60, 6, b, 12);

  uint16_t y = 76;
  if (tama.nAgents == 0) {
    centered(y, tama.connected ? "no sessions" : "waiting for bridge", 16);
    y += 20;
  } else {
    for (uint8_t i = 0; i < tama.nAgents && i < MAX_AGENTS; i++) {
      clipped(COL_NAME,  y, tama.agents[i].name,   14, 16);
      clipped(COL_STATE, y, tama.agents[i].status,  5, 16);
      clipped(COL_TOOL,  y, tama.agents[i].tool,   12, 16);
      y += 20;
    }
  }

  EPD_DrawLine(0, y + 4, UI_W - 1, y + 4, BLACK);

  // ---- transcript tail, filling whatever is left ----
  // Newest last, matching the wire order (protocol.py sends oldest-first).
  uint16_t ty = y + 12;
  const uint16_t tyMax = UI_H - 14;
  for (uint8_t i = 0; i < tama.nLines; i++) {
    if (ty > tyMax) break;
    clipped(COL_NAME, ty, tama.lines[i], 65, 12);
    ty += 14;
  }
}

// ------------------------------------------------- render change tracking -
// One signature integer per "thing worth a refresh"; render when it moves.
static uint32_t drawnSig = 0xFFFFFFFF;
static PersonaState lastBase = P_SLEEP;
static uint32_t celebrateUntil = 0;
static uint32_t lastFrameMs = 0;
static uint32_t renders = 0;          // reported in [alive]; refresh-rate check

// Minimum gap between panel pushes. Every frame is now a full ~2s refresh, so
// this is both a settle floor and a flash-rate limit: an idle board still
// redraws on the minute tick, and a busy one can't strobe.
const uint32_t MIN_FRAME_MS = 12000;

// FNV-1a over the agent rows — the list changing is what matters, and the
// counts alone miss a same-count state swap (one agent finishing as another
// starts), which is exactly the transition worth redrawing for.
//
// Name and status ONLY. The tool column is deliberately excluded: it changes
// on every single tool call, and hashing it in means a ~1s partial refresh
// per Bash/Read/Edit across every live session. That is far faster than the
// panel settles, so the screen never resolves into a readable frame — it just
// ghosts. Tool text rides along on the next structural change or minute tick,
// the same way the portrait build let transcript updates ride the tick.
static uint32_t agentsSigHash() {
  uint32_t h = 2166136261u;
  for (uint8_t i = 0; i < tama.nAgents && i < MAX_AGENTS; i++) {
    for (const char* p = tama.agents[i].name;   *p; p++) { h ^= (uint8_t)*p; h *= 16777619u; }
    for (const char* p = tama.agents[i].status; *p; p++) { h ^= (uint8_t)*p; h *= 16777619u; }
    h ^= 0xFF; h *= 16777619u;   // row separator, so {"ab",""} != {"a","b"}
  }
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

  // Held low so a stray float can't look like a press. Nothing reads them in
  // this build — the monitor is output-only — but leaving them configured
  // keeps the pin state identical to the interactive build.
  pinMode(PIN_BTN_MENU, INPUT);   // external pull-ups on the board
  pinMode(PIN_BTN_EXIT, INPUT);
  pinMode(PIN_BTN_NEXT, INPUT);
  pinMode(PIN_BTN_OK,   INPUT);
  pinMode(PIN_BTN_PREV, INPUT);

  pinMode(PIN_EPD_POWER, OUTPUT);
  digitalWrite(PIN_EPD_POWER, HIGH);
  EPD_GPIOInit();
  EPD_Clear();          // one true full clear at boot; also inits the panel

  Serial.println("[boot] claude_pet_eink monitor up (landscape)");
}

// ---------------------------------------------------------------- loop ----
void loop() {
  pollSerial();

  uint32_t now = millis();
  tama.connected = dataConnected();

  static uint32_t lastAlive = 0;
  if (now - lastAlive >= 5000) {
    lastAlive = now;
    char b[96];
    snprintf(b, sizeof(b), "[alive] up=%lus heap=%u state=%s agents=%u renders=%lu",
             now / 1000, ESP.getFreeHeap(), stateNames[lastBase], tama.nAgents,
             (unsigned long)renders);
    Serial.println(b);
  }

  // The host sends {"time":[...]} exactly once, on connect — and opening the
  // CH340 port toggles DTR/RTS, which resets this board. So the sync can land
  // while we are still in the ROM bootloader and be lost for good: heartbeats
  // repeat and recover on their own, the clock does not, and it sits at
  // "--:--" until something else reboots us. Re-emit the boot banner while we
  // have no clock — serial_transport's _IS_BOOT detector reads it as a reboot
  // and replays the full resync (time + heartbeat + status). Self-limiting:
  // the moment a time sync lands this goes quiet.
  static uint32_t lastClockRequestMs = 0;
  if (!timeValid && tama.connected && now - lastClockRequestMs >= 15000) {
    lastClockRequestMs = now;
    Serial.println("[boot] claude_pet_eink monitor: no clock yet, requesting resync");
  }

  // celebrate is a one-shot banner: entering it starts an 8s window,
  // afterwards the base state shows even if `completed` is still set
  PersonaState base = derive();
  if (base == P_CELEBRATE) {
    if (lastBase != P_CELEBRATE) celebrateUntil = now + 8000;
    else if ((int32_t)(now - celebrateUntil) >= 0) base = tama.sessionsRunning >= 3 ? P_BUSY : P_IDLE;
  }
  lastBase = base;

  uint8_t minute = 0xFF;
  if (timeValid) { struct tm lt; time_t n = nowLocal(); gmtime_r(&n, &lt); minute = lt.tm_min; }

  uint32_t sig = ((uint32_t)base << 0) | ((uint32_t)tama.connected << 5)
               | ((uint32_t)minute << 6) | ((uint32_t)tama.sessionsTotal << 12)
               | ((uint32_t)tama.sessionsRunning << 18) | ((uint32_t)tama.sessionsWaiting << 24);
  sig ^= agentsSigHash();

  // Floor on how often the panel may be pushed. A burst of session changes
  // (a swarm starting, several turns ending at once) would otherwise queue
  // back-to-back ~1s refreshes and leave the screen mid-transition the whole
  // time. This DEFERS rather than drops: sig still differs from drawnSig, so
  // the next eligible pass renders the newest state.
  if (sig != drawnSig && (lastFrameMs == 0 || now - lastFrameMs >= MIN_FRAME_MS)) {
    drawStatusScreen(base);
    pushFrame();
    drawnSig = sig;
    lastFrameMs = millis();
    renders++;
  }

  delay(10);
}
