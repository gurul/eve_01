#pragma once
// eve's chirps on the Voice PE speaker. The phrase recipes are the
// StackChan ones (firmware/claude_pet_stackchan/src/chirp.cpp, after Marcelo
// Larios' R2D2 Sound Generator, BSD); only the output path differs: a
// FreeRTOS task renders each phrase at 16 kHz and writes it, upsampled x3, to
// the 48 kHz AIC3204 bus. The amplifier stays on, as in ESPHome.
#include <stddef.h>
#include <stdint.h>

enum ChirpKind : uint8_t {
  CHIRP_WAKE,        // short rising whistle
  CHIRP_ATTENTION,   // phrase + excited beeps: a session waits on you
  CHIRP_HAPPY,       // rising trill: a task finished
  CHIRP_LISTEN,      // "hm?" up-chirp: listening
  CHIRP_OK,          // beep-boop: button pressed Enter
  CHIRP_NO,          // descending boop: error
  CHIRP_CURIOUS,     // two rising notes: button pressed with nothing waiting
  CHIRP_TICK,        // one short blip: dial step
  CHIRP_TONE,        // diagnostic sine, see chirpTone()
};

// Starts the audio task. Call after sbCodecBegin() and sbI2sSpeakerBegin().
void chirpBegin();
void chirpSetEnabled(bool on);
// 0..10. 0 is silent.
void chirpSetVolume(uint8_t level);
// Queues a phrase. Dropped when the queue is full or chirps are off.
void chirpPlay(ChirpKind kind);

// Diagnostics ({"cmd":"tone"} / {"cmd":"amp"}). A tone plays even when
// chirps are off, and logs "[chirp] played ..." with the I2S write result.
void chirpTone(uint16_t hz, uint16_t ms, float level);
void chirpForceAmp(bool on);

// eve's voice ({"cmd":"pcm"}): 24 kHz mono int16 from the daemon, played at the
// dial's volume. It plays whatever the sound setting says: sound off silences only
// the chirps. Write from one task only (the serial loop).
size_t chirpVoiceWrite(const int16_t *samples, size_t count);
void chirpVoiceFlush();
uint32_t chirpVoiceDropped();
