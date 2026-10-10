#pragma once
// R2D2-style chirps for the StackChan pet, after Marcelo Larios'
// R2D2 Sound Generator (vendor/stackchan/R2D2-Sound-Generator, BSD):
//   phrase1 = descending 2 Hz/step sweep then rising 10 Hz/step sweep from a
//             random base 1000..2000 Hz, ~1-2 ms per step
//   phrase2 = the mirror
//   beeps   = 70..170 ms at 2000 + (-1700..2000) Hz with 0..30 ms gaps
// Each phrase is synthesised into a PCM buffer (16 kHz, 8-bit unsigned,
// square wave through a phase accumulator, 2 ms attack/release per segment)
// and handed to M5.Speaker.playRaw() asynchronously. Speaker startup/shutdown
// may briefly wait for the driver; the idle drain timer never delays the loop.
// Plain header: no M5 types, safe for board_compat.cpp and body.cpp.
#include <stdint.h>

enum ChirpKind : uint8_t {
  CHIRP_WAKE,        // short rising whistle: SLEEP -> anything
  CHIRP_SLEEPY,      // descending phrase1, quiet: entering SLEEP
  CHIRP_ATTENTION,   // phrase2 + 4-6 excited beeps: entering ATTENTION, every 30 s after
  CHIRP_HAPPY,       // trill, 5-8 rising beeps: CELEBRATE, HEART
  CHIRP_LISTEN,      // one short "hm?" up-chirp: listening on
  CHIRP_OK,          // "beep-boop": card approved from the board
  CHIRP_NO,          // descending "boop": card denied
  CHIRP_CONFUSED,    // wobble: DIZZY
  // Mood engine (mood.cpp) while exploring, one per feeling change:
  CHIRP_CURIOUS,     // two rising notes: "oh?"
  CHIRP_SURPRISE,    // one high blip: "!"
  CHIRP_SIGH,        // slow falling sweep: bored / lonely
  CHIRP_WARBLE,      // soft quick warble: happy / affection
  CHIRP_STARTLE,     // sharp high double-blip: startled
  CHIRP_TALK,        // beep-boop babble: eve "talking" while a caption streams
};

// Allocates two 32 KB PSRAM buffers (double-buffered so a forced phrase never
// rewrites the buffer the speaker task is still reading).
void chirpBegin();
// Mute switch (settings().sound). Mute stops playback and disables the amp;
// muted requests are dropped silently. Unmute waits for a sound to start it.
void chirpSetEnabled(bool on);
// Synthesise + start a phrase. Dropped while another phrase plays unless
// `force`, which stops the current one first. Random base per call.
void chirpPlay(ChirpKind kind, bool force = false);
// Plain note (replaces BeepCompat::tone): freq Hz for ms, same drop rule.
void chirpBeep(uint16_t freq, uint16_t ms);
// Call once per loop(): disable the amp after the idle DMA tail has drained.
void chirpUpdate();
bool chirpPlaying();
