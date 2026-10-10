#pragma once
#include <Arduino.h>
#include <ArduinoJson.h>
#include "ble_bridge.h"
#include "xfer.h"
#include "persona.h"
#include "motion.h"
#include "expression.h"

struct TamaState {
  expression::Request expressionRequest;
  bool expressionPending;
  uint8_t  sessionsTotal;
  uint8_t  sessionsRunning;
  uint8_t  sessionsWaiting;
  bool     recentlyCompleted;
  uint32_t tokensToday;
  uint32_t lastUpdated;
  char     msg[24];
  bool     connected;
  char     lines[8][92];
  uint8_t  nLines;
  uint16_t lineGen;          // bumps when lines change — lets UI reset scroll
                             // Sized for the text-first card: ~21 rows of 36
                             // chars, matching the bridge's 720-byte cap.
  bool     listening;        // host dictation key held: {"cmd":"listen","on":bool}
  bool     ownerReset;       // {"cmd":"owner","op":"reset"} arrived; consumer clears it
  // Host vision ({"cmd":"cam"}, {"cmd":"face"}, {"cmd":"look"}, {"cmd":"mode"})
  bool     camOn;            // host wants the frame stream
  uint8_t  camFps;           // frames per second cap (default 5)
  uint16_t camW, camH;       // requested frame size (default 160x120)
  uint32_t faceSeq;          // frame seq the detection belongs to
  int8_t   faceBx, faceBy;   // -100..100, +bx = right of frame, +by = down
  uint8_t  faceSize;         // 0..100
  uint8_t  faceConf;         // 0..100, 0 = no face in that frame
  int16_t  faceYaw, facePitch; // head pose echoed back from the frame line
  bool     faceOwner;        // "who":"owner" (else unknown)
  uint32_t faceAtMs;         // millis() when the last face cmd arrived (0 = never)
  bool     hostLookReq;      // {"cmd":"look"} pending; consumer clears it
  bool     snapReq;          // {"cmd":"snap"}: one full-size photo; consumer (main -> look) clears it
  int16_t  hostLookYaw, hostLookPitch;
  uint16_t hostLookHold;     // ms
  bool     explore;          // {"cmd":"mode","explore":true}
  // {"cmd":"move",...}: a named motion the BOARD runs, not a pose. One of
  // three shapes, all one line: "osc" (a rhythm), "keys" (a path), "stop".
  uint8_t  moveReq;          // 0 none, 1 osc, 2 keys, 3 stop; consumer clears it
  motion::Osc moveOsc;       // shape 1, straight off the wire (admitted on the board)
  bool     moveCentreHere;   // shape 1: centre on the current pose (no centre given)
  motion::Key moveKeys[motion::kKeysMax];   // shape 2
  uint8_t  moveNKeys;
  // Voice / computer-control conversation on the host: {"cmd":"agent","state":".."}
  uint8_t  agentState;       // AgentState (persona.h); AG_IDLE = none
  uint32_t agentAtMs;        // millis() of the last agent cmd
  // Eve's reply, one pre-wrapped page at a time ({"cmd":"caption","page","of","lines":[..],
  // "hold_ms","chirp","final"}; {"cmd":"caption","clear":true} removes it). The host paces it.
  static constexpr uint8_t CAP_MAX_LINES = 6, CAP_MAX_COLS = 26;   // storage; the host sends <= 4 x 17
  char     captionLines[CAP_MAX_LINES][CAP_MAX_COLS + 1];
  uint8_t  captionNLines;    // 0 = nothing to show
  uint8_t  captionPage, captionOf;   // of == 0: page count not known yet (still streaming)
  uint16_t captionHoldMs;    // host's planned hold, clamped 500..15000; board expires at hold + grace
  bool     captionFinal, captionChirp;
  uint32_t captionAtMs;      // millis() of the last caption cmd with lines (0 = none)
  // Host appraisal of what the camera saw: {"cmd":"emote","dv":..,"da":..,"label":".."}
  bool     emoteReq;         // pending; the consumer (main.cpp -> mood) clears it
  int8_t   emoteDv, emoteDa; // -100..100
  char     emoteLabel[12];
};

// ---------------------------------------------------------------------------
// Three modes, checked in priority order:
//   demo   → auto-cycle fake scenarios every 8s, ignore live data
//   live   → JSON arrived in the last 10s over USB or BT
//   asleep → no data, all zeros, "No Claude connected"
// ---------------------------------------------------------------------------

static uint32_t _lastLiveMs = 0;
static uint32_t _lastBtByteMs = 0;   // hasClient() lies; track actual BT traffic
static bool     _demoMode   = false;
static uint8_t  _demoIdx    = 0;
static uint32_t _demoNext   = 0;

struct _Fake { const char* n; uint8_t t,r,w; bool c; uint32_t tok; };
static const _Fake _FAKES[] = {
  {"asleep",0,0,0,false,0}, {"one idle",1,0,0,false,12000},
  {"busy",4,3,0,false,89000}, {"attention",2,1,1,false,45000},
  {"completed",1,0,0,true,142000},
};

inline void dataSetDemo(bool on) {
  _demoMode = on;
  if (on) { _demoIdx = 0; _demoNext = millis(); }
}
inline bool dataDemo() { return _demoMode; }

inline bool dataConnected() {
  return _lastLiveMs != 0 && (millis() - _lastLiveMs) <= 30000;
}

inline bool dataBtActive() {
  // Desktop's idle keepalive is ~10s; give it 1.5x headroom.
  return _lastBtByteMs != 0 && (millis() - _lastBtByteMs) <= 15000;
}

inline const char* dataScenarioName() {
  if (_demoMode) return _FAKES[_demoIdx].n;
  if (dataConnected()) return dataBtActive() ? "bt" : "usb";
  return "none";
}

// Set true once the bridge sends a time sync — until then the RTC may
// hold whatever was on the coin cell (or 2000-01-01 if it lost power).
static bool _rtcValid = false;
inline bool dataRtcValid() { return _rtcValid; }

static void _applyJson(const char* line, TamaState* out) {
  JsonDocument doc;
  if (deserializeJson(doc, line)) return;
  // {"cmd":"listen","on":true|false}: the user holds the dictation key on
  // the host. Checked before xferCommand(), which swallows unknown cmds.
  const char* cmd = doc["cmd"];
  if (cmd && strcmp(cmd, "expression") == 0) {
    if (!doc["id"].is<uint32_t>() || !doc["id"].as<uint32_t>()) return;
    bool clear = doc["clear"].is<bool>() && doc["clear"].as<bool>();
    expression::Kind kind = expression::parse(doc["label"] | "");
    if (!clear && kind == expression::None) return;
    expression::Request req;
    req.id = doc["id"].as<uint32_t>();
    req.kind = clear ? expression::None : kind;
    req.ttl = doc["ttl_ms"].is<uint32_t>() ? doc["ttl_ms"].as<uint32_t>() : 4000;
    out->expressionRequest = req;
    out->expressionPending = true;
    _lastLiveMs = millis();
    return;
  }

  if (cmd && strcmp(cmd, "listen") == 0) {
    out->listening = doc["on"] | false;
    _lastLiveMs = millis();
    return;
  }
  // {"cmd":"owner","op":"reset"}: forget the remembered owner position.
  if (cmd && strcmp(cmd, "owner") == 0) {
    const char* op = doc["op"];
    if (op && strcmp(op, "reset") == 0) out->ownerReset = true;
    _lastLiveMs = millis();
    return;
  }
  // Host vision. {"cmd":"cam","on":bool,"fps":n,"w":160,"h":120}
  if (cmd && strcmp(cmd, "cam") == 0) {
    out->camOn  = doc["on"]  | false;
    out->camFps = doc["fps"] | (uint8_t)5;
    out->camW   = doc["w"]   | (uint16_t)160;
    out->camH   = doc["h"]   | (uint16_t)120;
    _lastLiveMs = millis();
    return;
  }
  // {"cmd":"face","seq":n,"bx":..,"by":..,"size":..,"conf":..,"yaw":..,"pitch":..,"who":"owner"|"unknown"}
  if (cmd && strcmp(cmd, "face") == 0) {
    out->faceSeq   = doc["seq"]   | (uint32_t)0;
    out->faceBx    = doc["bx"]    | (int8_t)0;
    out->faceBy    = doc["by"]    | (int8_t)0;
    out->faceSize  = doc["size"]  | (uint8_t)0;
    out->faceConf  = doc["conf"]  | (uint8_t)0;
    out->faceYaw   = doc["yaw"]   | (int16_t)0;
    out->facePitch = doc["pitch"] | (int16_t)45;
    const char* who = doc["who"];
    out->faceOwner = who && strcmp(who, "owner") == 0;
    out->faceAtMs  = millis();
    _lastLiveMs = millis();
    return;
  }
  // {"cmd":"look","yaw":deg,"pitch":deg,"hold":ms}: absolute pose request.
  if (cmd && strcmp(cmd, "look") == 0) {
    out->hostLookYaw   = doc["yaw"]   | (int16_t)0;
    out->hostLookPitch = doc["pitch"] | (int16_t)45;
    out->hostLookHold  = doc["hold"]  | (uint16_t)3000;
    out->hostLookReq   = true;
    _lastLiveMs = millis();
    return;
  }
  // {"cmd":"move","kind":"osc","yaw_amp":d,"pitch_amp":d,"period_ms":ms,
  //   "pitch_period_ms":ms,"phase_deg":d,"cycles":n,"dwell_pct":n,"jitter_pct":n,
  //   ["center_yaw":d,"center_pitch":d]}   — a rhythm the board runs by itself
  // {"cmd":"move","kind":"keys","keys":[[at_ms,yaw,pitch,speed],..<=8]}  — a path
  // {"cmd":"move","kind":"stop"}                                         — stop now
  // Every number is admitted on the board (motion.h) before a servo sees it, so
  // a host or a model asking for something silly is reduced, never obeyed.
  if (cmd && strcmp(cmd, "move") == 0) {
    _lastLiveMs = millis();
    const char* kind = doc["kind"] | "osc";
    if (strcmp(kind, "stop") == 0) { out->moveReq = 3; return; }
    if (strcmp(kind, "keys") == 0) {
      JsonArrayConst ks = doc["keys"].as<JsonArrayConst>();
      uint8_t n = 0;
      for (JsonArrayConst k : ks) {
        if (n >= motion::kKeysMax) break;
        out->moveKeys[n].atMs  = k[0] | (uint16_t)0;
        out->moveKeys[n].yaw   = k[1] | (int8_t)motion::KEEP;
        out->moveKeys[n].pitch = k[2] | (int8_t)motion::KEEP;
        out->moveKeys[n].speed = k[3] | (uint16_t)500;
        n++;
      }
      out->moveNKeys = n;
      out->moveReq = n ? 2 : 0;
      return;
    }
    motion::Osc o{};
    o.ampYaw        = (uint8_t)(doc["yaw_amp"]   | 0);
    o.ampPitch      = (uint8_t)(doc["pitch_amp"] | 0);
    o.periodMs      = doc["period_ms"]       | (uint16_t)900;
    o.pitchPeriodMs = doc["pitch_period_ms"] | (uint16_t)0;
    o.phaseDeg      = doc["phase_deg"]       | (int16_t)0;
    o.cycles        = (uint8_t)(doc["cycles"] | 4);
    o.dwellPct      = (uint8_t)(doc["dwell_pct"]  | 8);
    o.jitterPct     = (uint8_t)(doc["jitter_pct"] | 8);
    const bool haveCentre = doc["center_yaw"].is<int>() || doc["center_pitch"].is<int>();
    o.centerYaw   = doc["center_yaw"]   | (int16_t)0;
    o.centerPitch = doc["center_pitch"] | (int16_t)45;
    out->moveOsc = o;
    out->moveCentreHere = !haveCentre;
    out->moveReq = 1;
    return;
  }
  // {"cmd":"snap"}: the host wants one full-resolution photo (a diary picture).
  // The look task answers with a single frame line carrying "snap":true.
  if (cmd && strcmp(cmd, "snap") == 0) {
    out->snapReq = true;
    _lastLiveMs = millis();
    return;
  }
  // {"cmd":"agent","state":"wake|listening|thinking|speaking|working|asking|done|error|idle"}
  if (cmd && strcmp(cmd, "agent") == 0) {
    out->agentState = agentStateFrom(doc["state"] | "idle");
    out->agentAtMs = millis();
    _lastLiveMs = millis();
    return;
  }
  // {"cmd":"caption","page":i,"of":n,"lines":[".."],"hold_ms":ms,"chirp":bool,"final":bool}: one
  // page of eve's reply, wrapped and paced by the host; {"cmd":"caption","clear":true} removes it.
  if (cmd && strcmp(cmd, "caption") == 0) {
    _lastLiveMs = millis();
    if (doc["clear"] | false) { out->captionNLines = 0; out->captionAtMs = 0; return; }
    JsonArrayConst ls = doc["lines"].as<JsonArrayConst>();
    uint8_t n = 0;
    for (JsonVariantConst v : ls) {
      if (n >= TamaState::CAP_MAX_LINES) break;
      strlcpy(out->captionLines[n], v | "", TamaState::CAP_MAX_COLS + 1);
      n++;
    }
    out->captionNLines = n;
    out->captionPage = doc["page"] | 0;
    out->captionOf   = doc["of"] | 0;
    uint32_t h = doc["hold_ms"] | 8000u;
    out->captionHoldMs = (uint16_t)(h < 500 ? 500 : h > 15000 ? 15000 : h);
    out->captionFinal = doc["final"] | false;
    out->captionChirp = doc["chirp"] | false;
    out->captionAtMs = n ? millis() : 0;
    return;
  }
  // {"cmd":"emote","dv":-100..100,"da":-100..100,"label":"curious"}: the host's
  // vision-LLM appraisal nudges the mood engine (clamped there to +-0.3).
  if (cmd && strcmp(cmd, "emote") == 0) {
    int dv = doc["dv"] | 0, da = doc["da"] | 0;
    out->emoteDv = (int8_t)(dv < -100 ? -100 : dv > 100 ? 100 : dv);
    out->emoteDa = (int8_t)(da < -100 ? -100 : da > 100 ? 100 : da);
    strlcpy(out->emoteLabel, doc["label"] | "", sizeof(out->emoteLabel));
    out->emoteReq = true;
    _lastLiveMs = millis();
    return;
  }
  // {"cmd":"mode","explore":bool}
  if (cmd && strcmp(cmd, "mode") == 0) {
    out->explore = doc["explore"] | out->explore;
    _lastLiveMs = millis();
    return;
  }
  if (xferCommand(doc)) { _lastLiveMs = millis(); return; }

  // Bridge sends {"time":[epoch_sec, tz_offset_sec]}; gmtime_r on the
  // adjusted epoch yields local components including weekday.
  JsonArray t = doc["time"];
  if (!t.isNull() && t.size() == 2) {
    time_t local = (time_t)t[0].as<uint32_t>() + (int32_t)t[1];
    struct tm lt; gmtime_r(&local, &lt);
    RTC_TimeTypeDef tm = { (uint8_t)lt.tm_hour, (uint8_t)lt.tm_min, (uint8_t)lt.tm_sec };
    RTC_DateTypeDef dt = { (uint8_t)lt.tm_wday, (uint8_t)(lt.tm_mon + 1),
                           (uint8_t)lt.tm_mday, (uint16_t)(lt.tm_year + 1900) };
    M5.Rtc.SetTime(&tm);
    M5.Rtc.SetDate(&dt);
    extern uint32_t _clkLastRead;
    _clkLastRead = 0;   // force re-read so _clkDt and _rtcValid agree
    _rtcValid = true;
    _lastLiveMs = millis();
    return;
  }

  out->sessionsTotal     = doc["total"]     | out->sessionsTotal;
  out->sessionsRunning   = doc["running"]   | out->sessionsRunning;
  out->sessionsWaiting   = doc["waiting"]   | out->sessionsWaiting;
  out->recentlyCompleted = doc["completed"] | false;
  uint32_t bridgeTokens = doc["tokens"] | 0;
  if (doc["tokens"].is<uint32_t>()) statsOnBridgeTokens(bridgeTokens);
  out->tokensToday = doc["tokens_today"] | out->tokensToday;
  const char* m = doc["msg"];
  if (m) { strncpy(out->msg, m, sizeof(out->msg)-1); out->msg[sizeof(out->msg)-1]=0; }
  JsonArray la = doc["entries"];
  if (!la.isNull()) {
    uint8_t n = 0;
    for (JsonVariant v : la) {
      if (n >= 8) break;
      const char* s = v.as<const char*>();
      strncpy(out->lines[n], s ? s : "", 91); out->lines[n][91]=0;
      n++;
    }
    if (n != out->nLines || (n > 0 && strcmp(out->lines[n-1], out->msg) != 0)) {
      out->lineGen++;
    }
    out->nLines = n;
  }
  out->lastUpdated = millis();
  _lastLiveMs = millis();
}

template<size_t N>
struct _LineBuf {
  char buf[N];
  uint16_t len = 0;
  void feed(Stream& s, TamaState* out) {
    while (s.available()) {
      char c = s.read();
      if (c == '\n' || c == '\r') {
        if (len > 0) { buf[len]=0; if (buf[0]=='{') _applyJson(buf, out); len=0; }
      } else if (len < N-1) {
        buf[len++] = c;
      }
    }
  }
};

// Sized so a heartbeat carrying the full 720-byte prompt detail plus entries
// still fits on one line — an overflowing line truncates and the WHOLE
// snapshot fails to parse, which would drop heartbeats exactly while a long
// prompt is up. (HWCDC RX buffer is 4096, see board_compat.)
static _LineBuf<2048> _usbLine, _btLine;

inline void dataPoll(TamaState* out) {
  uint32_t now = millis();

  if (_demoMode) {
    if (now >= _demoNext) { _demoIdx = (_demoIdx + 1) % 5; _demoNext = now + 8000; }
    const _Fake& s = _FAKES[_demoIdx];
    out->sessionsTotal=s.t; out->sessionsRunning=s.r; out->sessionsWaiting=s.w;
    out->recentlyCompleted=s.c; out->tokensToday=s.tok; out->lastUpdated=now;
    out->connected = true;
    snprintf(out->msg, sizeof(out->msg), "demo: %s", s.n);
    return;
  }

  _usbLine.feed(Serial, out);
  // BLE ring buffer is drained manually since it's not a Stream.
  while (bleAvailable()) {
    int c = bleRead();
    if (c < 0) break;
    _lastBtByteMs = millis();
    if (c == '\n' || c == '\r') {
      if (_btLine.len > 0) {
        _btLine.buf[_btLine.len] = 0;
        if (_btLine.buf[0] == '{') _applyJson(_btLine.buf, out);
        _btLine.len = 0;
      }
    } else if (_btLine.len < sizeof(_btLine.buf) - 1) {
      _btLine.buf[_btLine.len++] = (char)c;
    }
  }

  out->connected = dataConnected();
  if (!out->connected) {
    out->sessionsTotal=0; out->sessionsRunning=0; out->sessionsWaiting=0;
    out->recentlyCompleted=false; out->lastUpdated=now; out->listening=false;
    out->camOn=false; out->explore=false; out->faceAtMs=0;
    strncpy(out->msg, "No Claude connected", sizeof(out->msg)-1);
    out->msg[sizeof(out->msg)-1]=0;
  }
}
