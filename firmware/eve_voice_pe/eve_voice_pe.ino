// eve on a Home Assistant Voice Preview Edition.
//
// The Voice PE has no screen, camera or motors, so this board is eve's
// lights, button, dial and chirps. It speaks the same newline-delimited JSON
// as firmware/claude_pet_stackchan over the native USB-Serial/JTAG port and
// drops the commands it cannot carry out (cam, look, move, face, caption...).
// The microphone stays on the Mac. eve's voice can stream here ({"cmd":"pcm"}). See docs/voice-pe.md.
//
// What the daemon needs from any board (bridge serial_transport.py, daemon.py):
//   - some bytes at least every 20 s: the [alive] line below, every 5 s
//   - an answer to {"cmd":"status"}, polled every 60 s
//
// Controls:
//   button hold  talk to eve, like the Telegram Mini App's call button; let go to send
//   button tap   while a session waits on you: Enter on the Mac (approve / pick)
//   dial         while a session waits on you: down / up through the choices
//   dial         otherwise: volume of eve's voice and chirps
//   mute switch  silences the chirps (the Mac mic is not affected)
//
// Spotify mode ({"cmd":"music_mode","on":true}, bridge music_mode.py): the ring breathes green and
//   1 / 2 / 3 clicks  play-pause / next / previous   ({"cmd":"music","clicks":n})
//   hold, let go      the device picker               ({"cmd":"music","hold":"short"})
//   hold 3 s          leave Spotify mode              ({"cmd":"music","hold":"long"})
//   dial              Spotify volume, or the picker   ({"cmd":"music","dial":d})
// The board only counts and times; the daemon does the Spotify part and sends the ring its levels.
#include <Arduino.h>
#include <ArduinoJson.h>
#include <Preferences.h>
#include <Wire.h>
#include <mbedtls/base64.h>

#include "chirp.h"
#include "ring.h"
#include "sb_codec.h"
#include "sb_config.h"
#include "sb_i2s.h"

#ifndef EVE_GIT_SHA
#define EVE_GIT_SHA "dev"
#endif

namespace {

// ---- state the daemon sends ----
struct Host {
  uint8_t total = 0, running = 0, waiting = 0;
  bool completed = false;
  uint32_t completedAtMs = 0;
  bool listening = false;          // {"cmd":"listen","on":..}
  char agent[12] = "idle";         // {"cmd":"agent","state":..}
  uint32_t agentAtMs = 0;
  bool sound = true;               // {"cmd":"sound","on":..}
  bool music = false;              // {"cmd":"music_mode","on":..}: Spotify mode
  uint32_t lastLiveMs = 0;         // last valid JSON line
} host;

Preferences prefs;
uint8_t volume = 6;

bool connected() { return host.lastLiveMs != 0 && millis() - host.lastLiveMs <= 30000; }

void send(const char *line) {
  Serial.print(line);
  Serial.print('\n');
}

void sendStatus() {
  // No "sec" (no BLE here), no "bat" (USB powered) and no "sys.fsTotal"
  // (no LittleFS): the daemon only reads those keys when present.
  char b[200];
  snprintf(b, sizeof(b),
           "{\"ack\":\"status\",\"ok\":true,\"n\":0,\"data\":{\"name\":\"eve\",\"owner\":\"\","
           "\"board\":\"voice-pe\",\"snd\":%s,\"sys\":{\"up\":%lu,\"heap\":%u}}}",
           host.sound ? "true" : "false", millis() / 1000, ESP.getFreeHeap());
  send(b);
}

void ack(const char *what) {
  char b[64];
  snprintf(b, sizeof(b), "{\"ack\":\"%s\",\"ok\":true,\"n\":0}", what);
  send(b);
}

// ---- incoming lines ----
void onAgent(const char *state) {
  if (strcmp(state, host.agent) == 0) return;
  strlcpy(host.agent, state, sizeof(host.agent));
  host.agentAtMs = millis();
  if (!strcmp(state, "wake")) chirpPlay(CHIRP_WAKE);
  else if (!strcmp(state, "listening")) chirpPlay(CHIRP_LISTEN);
  else if (!strcmp(state, "asking")) chirpPlay(CHIRP_ATTENTION);
  else if (!strcmp(state, "done")) chirpPlay(CHIRP_HAPPY);
  else if (!strcmp(state, "error")) chirpPlay(CHIRP_NO);
}

void onHeartbeat(JsonDocument &doc) {
  uint8_t waitingBefore = host.waiting;
  host.total = doc["total"] | 0;
  host.running = doc["running"] | 0;
  host.waiting = doc["waiting"] | 0;
  bool completed = doc["completed"] | false;
  if (completed && !host.completed) {
    chirpPlay(CHIRP_HAPPY);
    host.completedAtMs = millis();
  }
  host.completed = completed;
  if (host.waiting > waitingBefore) chirpPlay(CHIRP_ATTENTION);
}

// {"cmd":"pcm","d":"<base64>"}: a piece of eve's voice (desk_call.BoardSpeaker), about 100 ms of
// 24 kHz mono int16. Decoded straight into the voice buffer, without the JSON parser: 10 lines a second.
constexpr char PCM_PREFIX[] = "{\"cmd\":\"pcm\",\"d\":\"";
alignas(4) uint8_t pcmBytes[12288];

bool handlePcm(const char *line, size_t len) {
  constexpr size_t n = sizeof(PCM_PREFIX) - 1;
  if (len < n + 2 || strncmp(line, PCM_PREFIX, n) != 0) return false;
  const char *b64 = line + n;
  const char *end = strchr(b64, '"');
  if (!end) return true;
  size_t out = 0;
  if (mbedtls_base64_decode(pcmBytes, sizeof(pcmBytes), &out, (const unsigned char *)b64, end - b64) == 0) {
    chirpVoiceWrite(reinterpret_cast<const int16_t *>(pcmBytes), out / 2);
  }
  host.lastLiveMs = millis();
  return true;
}

// "rgb":[r,g,b] as 0xRRGGBB; absent or malformed is 0xFFFFFFFF, the ring's default colour.
uint32_t rgbOf(JsonVariantConst v) {
  if (!v.is<JsonArrayConst>() || v.size() != 3) return 0xFFFFFFFF;
  return ((uint32_t)(v[0] | 0) & 0xFF) << 16 | ((uint32_t)(v[1] | 0) & 0xFF) << 8 | ((uint32_t)(v[2] | 0) & 0xFF);
}

void handleLine(const char *line) {
  if (line[0] != '{') return;
  if (handlePcm(line, strlen(line))) return;
  JsonDocument doc;
  if (deserializeJson(doc, line)) return;
  host.lastLiveMs = millis();

  const char *cmd = doc["cmd"];
  if (!cmd) {
    if (doc["total"].is<int>()) onHeartbeat(doc);
    return;  // {"time":[..]} and others: liveness only
  }
  if (!strcmp(cmd, "status")) return sendStatus();
  if (!strcmp(cmd, "pcm_flush")) return chirpVoiceFlush();
  if (!strcmp(cmd, "agent")) return onAgent(doc["state"] | "idle");
  if (!strcmp(cmd, "music_mode")) {
    bool on = doc["on"] | false;
    if (on != host.music) chirpPlay(on ? CHIRP_OK : CHIRP_TICK);
    host.music = on;
    return;
  }
  if (!strcmp(cmd, "ring_level")) {
    ringShowLevel(doc["n"] | 0, doc["of"] | 10, doc["dot"] | false, doc["ms"] | 1500, rgbOf(doc["rgb"]));
    return;
  }
  if (!strcmp(cmd, "music_flash")) {
    bool ok = doc["ok"] | false;
    ringFlash(ok, rgbOf(doc["rgb"]));
    chirpPlay(ok ? CHIRP_OK : CHIRP_NO);
    return;
  }
  if (!strcmp(cmd, "listen")) {
    bool on = doc["on"] | false;
    if (on && !host.listening) chirpPlay(CHIRP_LISTEN);
    host.listening = on;
    return;
  }
  if (!strcmp(cmd, "sound")) {
    host.sound = doc["on"] | true;
    prefs.putBool("sound", host.sound);
    return ack("sound");
  }
  if (!strcmp(cmd, "led") || !strcmp(cmd, "name") || !strcmp(cmd, "owner") || !strcmp(cmd, "species") ||
      !strcmp(cmd, "unpair")) {
    return ack(cmd);
  }
  // Audio bring-up diagnostics, sent by hand (docs/voice-pe.md#audio-diagnostics).
  if (!strcmp(cmd, "tone")) {
    chirpTone(doc["hz"] | 440, doc["ms"] | 1000, doc["level"] | 0.5f);
    return;
  }
  if (!strcmp(cmd, "amp")) {
    chirpForceAmp(doc["on"] | false);
    Serial.printf("[amp] forced %s\n", (doc["on"] | false) ? "on" : "off");
    return;
  }
  if (!strcmp(cmd, "dac")) {
    // {"cmd":"dac","page":0,"reg":65,"val":0}: val omitted = read only.
    uint8_t page = doc["page"] | 0, reg = doc["reg"] | 0;
    Wire.beginTransmission(0x18);
    Wire.write(0x00);
    Wire.write(page);
    bool ok = Wire.endTransmission() == 0;
    if (!doc["val"].isNull()) {
      Wire.beginTransmission(0x18);
      Wire.write(reg);
      Wire.write((uint8_t)(doc["val"].as<int>()));
      ok = ok && Wire.endTransmission() == 0;
    }
    Wire.beginTransmission(0x18);
    Wire.write(reg);
    ok = ok && Wire.endTransmission(false) == 0;
    int v = (Wire.requestFrom(0x18, 1) == 1) ? Wire.read() : -1;
    Serial.printf("[dac] page %u reg 0x%02X = 0x%02X (i2c %s)\n", page, reg, v & 0xFF, ok && v >= 0 ? "ok" : "FAIL");
    return;
  }
  // cam, snap, face, look, move, mode, expression, emote, caption, char_*:
  // this board has no camera, motors or screen.
}

char lineBuf[16384];
size_t lineLen = 0;
bool lineOverflow = false;

void pollSerial() {
  // Bulk reads: eve's voice arrives at about 64 KB/s.
  uint8_t chunk[1024];
  int avail;
  while ((avail = Serial.available()) > 0) {
    size_t n = Serial.read(chunk, avail < (int)sizeof(chunk) ? avail : sizeof(chunk));
    for (size_t i = 0; i < n; i++) {
      char c = (char)chunk[i];
      if (c == '\n') {
        if (!lineOverflow) {
          lineBuf[lineLen] = 0;
          handleLine(lineBuf);
        }
        lineLen = 0;
        lineOverflow = false;
      } else if (c != '\r') {
        if (lineLen < sizeof(lineBuf) - 1) lineBuf[lineLen++] = c;
        else lineOverflow = true;  // drop the whole line, never half of it
      }
    }
  }
}

// ---- button ----
// Hold to talk, like the Telegram Mini App's call button: {"cmd":"ptt","on":true} on press,
// {"cmd":"ptt","on":false} on release; the daemon records the Mac mic in between (desk_call.py).
// While a session waits on you, a quick tap is Enter instead, and talking starts after TALK_AFTER_MS.
constexpr uint32_t TALK_AFTER_MS = 400;
bool btnDown = false, talking = false;
uint32_t btnDownMs = 0, btnChangeMs = 0;

void startTalking() {
  talking = true;
  send("{\"cmd\":\"ptt\",\"on\":true}");
  Serial.println("[btn] talk");
}

// Spotify mode: clicks are counted until CLICK_GAP_MS passes with no new press; a press held HOLD_MS is a
// hold (the picker, on release), and one held LEAVE_MS leaves the mode at once.
constexpr uint32_t CLICK_GAP_MS = 350, HOLD_MS = 600, LEAVE_MS = 3000;
uint8_t clicks = 0;
uint32_t lastClickMs = 0;
bool holdTicked = false, leaveSent = false;

void musicButton(bool changed, uint32_t now) {
  char b[48];
  if (changed && btnDown) {
    btnDownMs = now;
    holdTicked = leaveSent = false;
  } else if (changed) {
    uint32_t held = now - btnDownMs;
    if (leaveSent) {
      // already sent on the hold
    } else if (held >= HOLD_MS) {
      clicks = 0;
      send("{\"cmd\":\"music\",\"hold\":\"short\"}");
      Serial.println("[btn] music hold");
    } else {
      clicks++;
      lastClickMs = now;
    }
  }
  if (btnDown && !holdTicked && now - btnDownMs >= HOLD_MS) {
    holdTicked = true;
    chirpPlay(CHIRP_TICK);  // "let go now for the picker"
  }
  if (btnDown && !leaveSent && now - btnDownMs >= LEAVE_MS) {
    leaveSent = true;
    clicks = 0;
    send("{\"cmd\":\"music\",\"hold\":\"long\"}");
    Serial.println("[btn] music leave");
  }
  if (!btnDown && clicks > 0 && now - lastClickMs >= CLICK_GAP_MS) {
    snprintf(b, sizeof(b), "{\"cmd\":\"music\",\"clicks\":%u}", clicks);
    send(b);
    Serial.printf("[btn] music %u click(s)\n", clicks);
    chirpPlay(CHIRP_TICK);
    clicks = 0;
  }
}

void pollButton() {
  bool down = digitalRead(SB_PIN_BUTTON) == LOW;
  uint32_t now = millis();
  if (host.music && connected() && !talking) {
    bool changed = down != btnDown && now - btnChangeMs > 25;
    if (changed) {
      btnChangeMs = now;
      btnDown = down;
    }
    return musicButton(changed, now);
  }
  clicks = 0;
  if (down != btnDown && now - btnChangeMs > 25) {
    btnChangeMs = now;
    btnDown = down;
    if (down) {
      btnDownMs = now;
      if (!connected()) {
        chirpPlay(CHIRP_NO);
      } else if (host.waiting == 0) {
        startTalking();
      }
    } else if (talking) {
      talking = false;
      send("{\"cmd\":\"ptt\",\"on\":false}");
      Serial.println("[btn] done");
    } else if (host.waiting > 0 && connected()) {  // a quick tap on a waiting prompt
      send("{\"cmd\":\"key\",\"name\":\"enter\"}");
      chirpPlay(CHIRP_OK);
      Serial.println("[btn] enter");
    }
  }
  if (btnDown && !talking && connected() && host.waiting > 0 && now - btnDownMs >= TALK_AFTER_MS) {
    startTalking();
  }
}

// ---- dial ----
// Quadrature decoder: a valid Gray-code step adds +-1; four steps per detent.
volatile int32_t dialSteps = 0;
volatile uint8_t dialPrev = 0;
const int8_t kQuad[16] = {0, -1, 1, 0, 1, 0, 0, -1, -1, 0, 0, 1, 0, 1, -1, 0};

void IRAM_ATTR onDial() {
  uint8_t cur = (digitalRead(SB_PIN_DIAL_A) << 1) | digitalRead(SB_PIN_DIAL_B);
  dialSteps += kQuad[(dialPrev << 2) | cur];
  dialPrev = cur;
}

int32_t dialUsed = 0;

void pollDial() {
  int32_t detents = dialSteps / 4 - dialUsed;
  if (detents == 0) return;
  dialUsed += detents;
  if (host.music && connected()) {
    // Spotify volume or the device picker: the daemon decides, and sends the ring its level
    char b[48];
    snprintf(b, sizeof(b), "{\"cmd\":\"music\",\"dial\":%ld}", (long)detents);
    send(b);
    return;
  }
  if (host.waiting > 0 && connected()) {
    // Clockwise moves down the list of choices.
    for (int32_t i = 0; i < abs(detents); i++) {
      send(detents > 0 ? "{\"cmd\":\"key\",\"name\":\"next\"}" : "{\"cmd\":\"key\",\"name\":\"prev\"}");
    }
    chirpPlay(CHIRP_TICK);
    return;
  }
  int32_t v = (int32_t)volume + detents;
  volume = (uint8_t)constrain(v, 0, 10);
  chirpSetVolume(volume);
  prefs.putUChar("vol", volume);
  ringShowLevel(volume);
  chirpPlay(CHIRP_TICK);
}

// ---- ring ----
bool agentActive() {
  // A conversation that never sent "idle" (daemon killed) stops showing
  // after 10 minutes.
  return strcmp(host.agent, "idle") != 0 && millis() - host.agentAtMs < 600000;
}

void updateRing() {
  bool muted = sbCodecMicMuted();
  ringSetMuted(muted);
  chirpSetEnabled(host.sound && !muted);

  if (!connected()) return ringSet(LOOK_OFFLINE);
  if (talking) return ringSet(LOOK_LISTENING);
  if (host.music) return ringSet(LOOK_MUSIC);
  if (agentActive()) {
    const char *a = host.agent;
    if (!strcmp(a, "wake") || !strcmp(a, "listening")) return ringSet(LOOK_LISTENING);
    if (!strcmp(a, "thinking")) return ringSet(LOOK_THINKING);
    if (!strcmp(a, "speaking")) return ringSet(LOOK_SPEAKING);
    if (!strcmp(a, "working")) return ringSet(LOOK_WORKING);
    if (!strcmp(a, "asking")) return ringSet(LOOK_WAITING);
    // done and error are flashes: 3 s, then the session state shows.
    bool recent = millis() - host.agentAtMs < 3000;
    if (!strcmp(a, "done") && recent) return ringSet(LOOK_DONE);
    if (!strcmp(a, "error") && recent) return ringSet(LOOK_ERROR);
  }
  if (host.listening) return ringSet(LOOK_LISTENING);
  if (host.waiting > 0) return ringSet(LOOK_WAITING);
  if (host.running > 0) return ringSet(LOOK_WORKING);
  if (host.completed && millis() - host.completedAtMs < 3000) return ringSet(LOOK_DONE);
  ringSet(LOOK_IDLE);
}

uint32_t lastAliveMs = 0;

}  // namespace

void setup() {
  Serial.setRxBufferSize(16384);
  Serial.begin(115200);
  ringBegin();

  prefs.begin("eve", false);
  host.sound = prefs.getBool("sound", true);
  volume = prefs.getUChar("vol", 6);
  chirpSetVolume(volume);

  pinMode(SB_PIN_BUTTON, INPUT_PULLUP);
  pinMode(SB_PIN_DIAL_A, INPUT_PULLUP);
  pinMode(SB_PIN_DIAL_B, INPUT_PULLUP);
  dialPrev = (digitalRead(SB_PIN_DIAL_A) << 1) | digitalRead(SB_PIN_DIAL_B);
  attachInterrupt(SB_PIN_DIAL_A, onDial, CHANGE);
  attachInterrupt(SB_PIN_DIAL_B, onDial, CHANGE);

  // The daemon resyncs on a line starting "[boot] ".
  Serial.printf("[boot] eve_voice_pe %s\n", EVE_GIT_SHA);

  bool codec = sbCodecBegin();  // ~2.5 s: DAC soft-step wait
  bool spk = codec && sbI2sSpeakerBegin();
  Serial.printf("[audio] codec=%d speaker=%d\n", codec, spk);
  chirpBegin();
  chirpSetEnabled(host.sound && !sbCodecMicMuted());
  chirpPlay(CHIRP_WAKE);
}

void loop() {
  pollSerial();
  pollButton();
  pollDial();
  updateRing();
  ringUpdate();

  uint32_t now = millis();
  if (now - lastAliveMs >= 5000) {
    lastAliveMs = now;
    Serial.printf("[alive] up=%lu heap=%u live=%d total=%u running=%u waiting=%u agent=%s voice_dropped=%lu\n",
                  now / 1000, ESP.getFreeHeap(), connected(), host.total, host.running, host.waiting, host.agent,
                  (unsigned long)chirpVoiceDropped());
  }
  delay(2);
}
