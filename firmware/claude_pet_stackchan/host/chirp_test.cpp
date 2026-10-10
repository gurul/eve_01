// Compile this with the real src/chirp.cpp and -Ihost/chirp_stubs.
#include <M5Unified.hpp>
#include "../src/chirp.h"
#include <cstdio>

uint32_t fakeMillis = 0;

int main() {
  auto& spk = M5.Speaker;
  chirpBegin();
  chirpUpdate();
  assert(!spk.running && spk.ends == 0);

  // Positive control: a real beep must start the speaker and remain powered
  // throughout playback, even long after the idle timeout would have expired.
  chirpBeep(1000, 100);
  assert(spk.running && chirpPlaying() && spk.plays == 1);
  fakeMillis = 1000;
  chirpUpdate();
  assert(spk.running && spk.ends == 0);
  chirpBeep(1200, 100);
  assert(spk.plays == 1); // overlapping ordinary sounds are still dropped

  spk.playing = false; // mixer finished; DMA still has the sound's tail
  chirpUpdate();
  fakeMillis += 149;
  chirpUpdate();
  assert(spk.running);
  ++fakeMillis;
  chirpUpdate();
  assert(!spk.running && spk.ends == 1);
  chirpUpdate();
  assert(spk.ends == 1); // idle ticks do not repeatedly tear down the driver

  chirpPlay(CHIRP_OK);
  assert(spk.running && chirpPlaying() && spk.plays == 2);
  chirpPlay(CHIRP_NO, true);
  assert(spk.plays == 3 && chirpPlaying());
  chirpSetEnabled(false);
  assert(!spk.running && !chirpPlaying() && spk.ends == 2);
  chirpPlay(CHIRP_OK, true);
  chirpBeep(1000, 100);
  chirpSetEnabled(false);
  assert(spk.plays == 3 && spk.ends == 2);
  chirpSetEnabled(true);
  assert(!spk.running); // unmute alone must not bring the hiss back

  // New playback during a drain cancels that drain, even without an update
  // observing the new playback before its mixer finishes.
  chirpBeep(1000, 20);
  spk.playing = false;
  chirpUpdate();
  fakeMillis += 140;
  chirpBeep(1000, 20);
  spk.playing = false;
  fakeMillis += 20;
  chirpUpdate();
  assert(spk.running);
  fakeMillis += 150;
  chirpUpdate();
  assert(!spk.running);

  // Timer rollover must preserve the tail rather than cutting it off.
  chirpBeep(1000, 20);
  spk.playing = false;
  fakeMillis = UINT32_MAX - 50;
  chirpUpdate();
  fakeMillis += 149;
  chirpUpdate();
  assert(spk.running);
  ++fakeMillis;
  chirpUpdate();
  assert(!spk.running);

  spk.beginOK = false;
  int playsBefore = spk.plays;
  chirpBeep(1000, 20);
  assert(!spk.running && spk.plays == playsBefore);
  spk.beginOK = true;
  spk.playOK = false;
  chirpBeep(1000, 20);
  chirpUpdate();
  fakeMillis += 150;
  chirpUpdate();
  assert(!spk.running);

  puts("chirp lifecycle passed");
}
