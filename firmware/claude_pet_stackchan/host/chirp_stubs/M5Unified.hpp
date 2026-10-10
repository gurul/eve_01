#pragma once
#include <cassert>
#include <cstddef>
#include <cstdint>
#include <cstdlib>

extern uint32_t fakeMillis;
inline uint32_t millis() { return fakeMillis; }
inline bool psramFound() { return true; }
inline long random(long hi) { return hi / 2; }
inline long random(long lo, long hi) { return lo + (hi - lo) / 2; }
struct SerialStub {
  template<class... Args> void printf(const char*, Args...) {}
  void println(const char*) {}
};
inline SerialStub Serial;
struct SpeakerStub {
  bool running = false, playing = false, beginOK = true, playOK = true;
  int ends = 0, plays = 0;
  bool begin() { running = beginOK; return beginOK; }
  void end() { ++ends; running = playing = false; }
  bool isRunning() const { return running; }
  bool isPlaying() const { return playing; }
  bool playRaw(const uint8_t* data, size_t len, uint32_t rate,
               bool stereo, uint32_t repeat, int channel, bool force) {
    if (!begin()) return true; // match the installed driver's lazy startup
    assert(running && data && len > 0 && len <= 32000);
    assert(rate == 16000 && !stereo && repeat == 1 && channel == -1);
    assert(!playing || force);
    ++plays;
    playing = playOK;
    return playOK;
  }
};
struct M5Stub { SpeakerStub Speaker; };
inline M5Stub M5;
