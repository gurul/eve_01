#pragma once
#include <Arduino.h>
#include <LittleFS.h>
#include <Preferences.h>
#include "ble_bridge.h"
#include "diag.h"
#include "hal_m5.h"
#include <mbedtls/base64.h>
#include <ArduinoJson.h>

static File     _xFile;
static uint32_t _xExpected = 0, _xWritten = 0;
static char     _xCharName[24] = "";
static bool     _xActive = false;
static uint32_t _xTotal = 0, _xTotalWritten = 0;

// Ack goes to both streams — we don't track which one delivered the command,
// and writes to a clientless SerialBT just drop. The bridge listens on
// whichever port it opened.
static void _xAck(const char* what, bool ok, uint32_t n = 0) {
  char b[64];
  int len = snprintf(b, sizeof(b), "{\"ack\":\"%s\",\"ok\":%s,\"n\":%lu}\n", what, ok?"true":"false", (unsigned long)n);
  Serial.write(b, len);
  bleWrite((const uint8_t*)b, len);
}

static uint32_t _xWipeDir(const char* dir) {
  File d = LittleFS.open(dir);
  if (!d || !d.isDirectory()) { LittleFS.mkdir(dir); return 0; }
  uint32_t freed = 0;
  File f = d.openNextFile();
  while (f) {
    freed += f.size();
    char p[80];
    snprintf(p, sizeof(p), "%s/%s", dir, f.name());
    f.close();
    LittleFS.remove(p);
    f = d.openNextFile();
  }
  d.close();
  return freed;
}

// Only one character lives on the device at a time. Installing a new one
// under a different name would otherwise leave the old one's files eating
// space. Wipe everything under /characters/, return total bytes reclaimed.
static uint32_t _xWipeAllChars() {
  File root = LittleFS.open("/characters");
  if (!root || !root.isDirectory()) { LittleFS.mkdir("/characters"); return 0; }
  uint32_t freed = 0;
  File sub = root.openNextFile();
  while (sub) {
    if (sub.isDirectory()) {
      char p[64];
      snprintf(p, sizeof(p), "/characters/%s", sub.name());
      sub.close();
      freed += _xWipeDir(p);
      LittleFS.rmdir(p);
    } else {
      sub.close();
    }
    sub = root.openNextFile();
  }
  root.close();
  return freed;
}

// Called from data.h when incoming JSON has a "cmd" key. Returns true if
// it was a transfer command (caller should skip state-update parsing).
// Needs characterClose()/characterInit() declared before this include.
void characterClose();
bool characterInit(const char* name);
void petNameSet(const char* name);
const char* petName();
void ownerSet(const char* name);
const char* ownerName();
#include "stats.h"
#include "board_compat.h"

inline bool xferCommand(JsonDocument& doc) {
  const char* cmd = doc["cmd"];
  if (!cmd) return false;

  if (strcmp(cmd, "diag") == 0) {
    diagReport("requested");
    return true;
  }

  if (strcmp(cmd, "name") == 0) {
    const char* n = doc["name"];
    if (n) petNameSet(n);
    _xAck("name", n != nullptr);
    return true;
  }

  if (strcmp(cmd, "species") == 0) {
    extern bool eveMode, gifAvailable;
    extern void eveSetSpeciesIdx(uint8_t);
    uint8_t idx = doc["idx"] | 0xFF;
    speciesIdxSave(idx);
    eveMode = !(gifAvailable && idx == 0xFF);
    if (eveMode) eveSetSpeciesIdx(idx);
    _xAck("species", true);
    return true;
  }

  if (strcmp(cmd, "unpair") == 0) {
    bleClearBonds();
    _xAck("unpair", true);
    return true;
  }

  if (strcmp(cmd, "owner") == 0) {
    const char* n = doc["name"];
    if (n) ownerSet(n);
    _xAck("owner", n != nullptr);
    return true;
  }

  // Timed bench isolation; does not change persisted user settings.
  if (strcmp(cmd, "noise_test") == 0) {
    _xAck("noise_test", halNoiseTest(doc["target"] | ""));
    return true;
  }

  // {"cmd":"sound","on":bool}: the owner muted or unmuted eve by voice.
  // Persisted (NVS "s_snd"), and main.cpp hands it to the chirp synth every
  // loop, so every chirp and beep follows it. Motion and LEDs do not.
  if (strcmp(cmd, "sound") == 0) {
    settings().sound = doc["on"] | true;
    settingsSave();
    _xAck("sound", true);
    return true;
  }

  // {"cmd":"led","on":bool}: back LEDs on or off. Persisted (NVS "s_led");
  // main.cpp hands it to bodyLedPolicy every loop.
  if (strcmp(cmd, "led") == 0) {
    settings().led = doc["on"] | true;
    settingsSave();
    _xAck("led", true);
    return true;
  }

  // {"cmd":"pitch_zero"} reports the pitch servo's zero (NVS servo/zero_pos_2, raw
  // servo steps, 3.2 per degree; BSP default 620). {"cmd":"pitch_zero","raw":N}
  // stores a new one and reboots, since the BSP reads it only at begin(). It
  // shifts the whole pitch range (a lower zero is a smaller servo angle; which
  // way that tilts depends on the saved axis setting, {"cmd":"axis"}). Added
  // 2026-09-27 while chasing a head that fell back up at rest; the zero itself
  // stays at the BSP default 620. Bounded to 520..700
  // so one command cannot drive the neck hard into its stop; step it while
  // someone watches the head.
  // {"cmd":"ext_power","on":bool}: the CoreS3 external 5 V output, not persisted
  // (every boot is quiet again). Acks with the readback and a fresh servo read in
  // tenths of a degree (n = yaw*10000 + pitch, both offset by 5000 to stay
  // unsigned), so a bench can tell a servo that answers from one that does not.
  if (strcmp(cmd, "ext_power") == 0) {
    bool on = doc["on"] | false;
    int y = 0, pt = 0;
    bool ok = halExtPower(on, &y, &pt);
    _xAck("ext_power", ok, (uint32_t)((y + 5000) * 10000 + (pt + 5000)));
    return true;
  }

  // {"cmd":"axis"} reports the head's axis directions; with "pitch_rev"/"yaw_rev" it sets and saves
  // them (body.cpp). n = pitch_rev*2 + yaw_rev. The host's head calibration page drives this.
  if (strcmp(cmd, "axis") == 0) {
    extern void bodyAxisGet(bool*, bool*);
    extern void bodySetAxis(bool, bool);
    bool pr = false, yr = false;
    bodyAxisGet(&pr, &yr);
    if (!doc["pitch_rev"].isNull()) pr = doc["pitch_rev"] | pr;
    if (!doc["yaw_rev"].isNull())   yr = doc["yaw_rev"] | yr;
    if (!doc["pitch_rev"].isNull() || !doc["yaw_rev"].isNull()) bodySetAxis(pr, yr);
    _xAck("axis", true, (uint32_t)(pr * 2 + yr));
    return true;
  }

  if (strcmp(cmd, "pitch_zero") == 0) {
    Preferences p;
    int cur = -1;
    if (p.begin("servo", true)) { cur = p.getInt("zero_pos_2", -1); p.end(); }
    if (doc["raw"].isNull()) { _xAck("pitch_zero", cur >= 0, (uint32_t)(cur < 0 ? 0 : cur)); return true; }
    int raw = doc["raw"] | -1;
    if (raw < 520 || raw > 700) { _xAck("pitch_zero", false, (uint32_t)(cur < 0 ? 0 : cur)); return true; }
    bool ok = p.begin("servo", false) && p.putInt("zero_pos_2", raw) == sizeof(int32_t);
    p.end();
    _xAck("pitch_zero", ok, (uint32_t)raw);
    if (ok) { Serial.flush(); delay(200); ESP.restart(); }
    return true;
  }

  if (strcmp(cmd, "status") == 0) {
    // Dump everything the info screens show. Manual printf rather than
    // ArduinoJson serialize — less heap churn, and the shape is fixed.
    int vBat = (int)(M5.Axp.GetBatVoltage() * 1000);
    int iBat = (int)M5.Axp.GetBatCurrent();
    int vBus = (int)(M5.Axp.GetVBusVoltage() * 1000);
    int pct = (vBat - 3200) / 10;
    if (pct < 0) pct = 0; if (pct > 100) pct = 100;
    char b[384];   // 320 left no room for a full-length name + owner once "snd" was added
    int len = snprintf(b, sizeof(b),
      "{\"ack\":\"status\",\"ok\":true,\"n\":0,\"data\":{"
      "\"name\":\"%s\",\"owner\":\"%s\",\"sec\":%s,\"snd\":%s,"
      "\"bat\":{\"pct\":%d,\"mV\":%d,\"mA\":%d,\"usb\":%s},"
      "\"sys\":{\"up\":%lu,\"heap\":%u,\"fsFree\":%lu,\"fsTotal\":%lu},"
      "\"stats\":{\"appr\":%u,\"deny\":%u,\"vel\":%u,\"nap\":%lu,\"lvl\":%u}"
      "}}\n",
      petName(), ownerName(), bleSecure() ? "true" : "false", settings().sound ? "true" : "false",
      pct, vBat, iBat, (vBus > 4000) ? "true" : "false",
      millis() / 1000, ESP.getFreeHeap(),
      (unsigned long)(LittleFS.totalBytes() - LittleFS.usedBytes()),
      (unsigned long)LittleFS.totalBytes(),
      stats().approvals, stats().denials, statsMedianVelocity(),
      (unsigned long)stats().napSeconds, stats().level
    );
    Serial.write(b, len);
    bleWrite((const uint8_t*)b, len);
    return true;
  }

  if (strcmp(cmd, "char_begin") == 0) {
    const char* name = doc["name"] | "pet";
    _xTotal = doc["total"] | 0;

    // Fit check: free space after wiping everything under /characters/.
    // Do the math before touching the filesystem so a failed check leaves
    // the current character intact.
    uint32_t free = LittleFS.totalBytes() - LittleFS.usedBytes();
    uint32_t reclaimable = 0;
    {
      File r = LittleFS.open("/characters");
      if (r && r.isDirectory()) {
        File s = r.openNextFile();
        while (s) {
          if (s.isDirectory()) {
            File f = s.openNextFile();
            while (f) { reclaimable += f.size(); f.close(); f = s.openNextFile(); }
          }
          s.close(); s = r.openNextFile();
        }
        r.close();
      }
    }
    // Headroom for LittleFS metadata overhead — it's not byte-for-byte.
    uint32_t available = free + reclaimable;
    if (_xTotal > 0 && _xTotal + 4096 > available) {
      char b[96];
      int len = snprintf(b, sizeof(b),
        "{\"ack\":\"char_begin\",\"ok\":false,\"n\":%lu,\"error\":\"need %luK, have %luK\"}\n",
        (unsigned long)available, (unsigned long)(_xTotal/1024), (unsigned long)(available/1024)
      );
      Serial.write(b, len);
      bleWrite((const uint8_t*)b, len);
      return true;
    }

    strncpy(_xCharName, name, sizeof(_xCharName)-1); _xCharName[sizeof(_xCharName)-1]=0;
    characterClose();
    _xWipeAllChars();
    char dir[48]; snprintf(dir, sizeof(dir), "/characters/%s", _xCharName);
    LittleFS.mkdir(dir);
    _xTotalWritten = 0;
    _xActive = true;
    _xAck("char_begin", true);
    return true;
  }

  if (!_xActive) return strcmp(cmd, "permission") != 0;  // permission cmd is not ours

  if (strcmp(cmd, "file") == 0) {
    const char* path = doc["path"];
    _xExpected = doc["size"] | 0;
    _xWritten = 0;
    if (!path) { _xAck("file", false); return true; }
    char full[80]; snprintf(full, sizeof(full), "/characters/%s/%s", _xCharName, path);
    _xFile = LittleFS.open(full, "w");
    _xAck("file", (bool)_xFile);
    return true;
  }

  if (strcmp(cmd, "chunk") == 0) {
    const char* b64 = doc["d"];
    if (!b64 || !_xFile) { _xAck("chunk", false); return true; }
    uint8_t buf[300];
    size_t outLen = 0;
    int rc = mbedtls_base64_decode(buf, sizeof(buf), &outLen,
                                   (const uint8_t*)b64, strlen(b64));
    if (rc != 0) { _xAck("chunk", false); return true; }
    _xFile.write(buf, outLen);
    _xWritten += outLen;
    _xTotalWritten += outLen;
    // Ack every chunk — LittleFS writes can block on flash erase and the
    // UART RX buffer is only ~256 bytes. Without this the sender overruns it.
    _xAck("chunk", true, _xWritten);
    return true;
  }

  if (strcmp(cmd, "file_end") == 0) {
    bool ok = _xFile && (_xWritten == _xExpected || _xExpected == 0);
    if (_xFile) _xFile.close();
    _xAck("file_end", ok, _xWritten);
    return true;
  }

  if (strcmp(cmd, "char_end") == 0) {
    _xActive = false;
    bool ok = characterInit(_xCharName);
    extern bool eveMode, gifAvailable;
    if (ok) { eveMode = false; gifAvailable = true; speciesIdxSave(0xFF); }
    _xAck("char_end", ok);
    return true;
  }

  return false;
}

inline bool xferActive() { return _xActive; }
inline uint32_t xferProgress() { return _xTotalWritten; }
inline uint32_t xferTotal() { return _xTotal; }
