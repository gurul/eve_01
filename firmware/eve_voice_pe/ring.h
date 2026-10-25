#pragma once
// The 12-LED ring is eve's face on the Voice PE: one look per state.
#include <stdint.h>

enum RingLook : uint8_t {
  LOOK_OFFLINE,    // no message from the daemon for 30 s: one dim dot circles
  LOOK_IDLE,       // connected, nothing running: asleep, three red "z" blips rise
  LOOK_WORKING,    // Claude sessions running, or eve doing a task: teal comet
  LOOK_THINKING,   // eve thinking: violet double comet
  LOOK_LISTENING,  // eve listening: soft blue, steady breathe
  LOOK_SPEAKING,   // eve speaking: blue, quick pulse
  LOOK_WAITING,    // a session waits on you: amber pulse
  LOOK_DONE,       // a task finished: green sweep (transient)
  LOOK_ERROR,      // red blink (transient)
  LOOK_MUSIC,      // Spotify mode (bridge music_mode.py): green, slow breathe
};

void ringBegin();
void ringSet(RingLook look);
// Shows `level` of `of` as lit LEDs for `ms`, over whatever look is on (green in Spotify mode). With `dot`,
// one LED marks the position instead (the device picker). ms = 0 clears it.
// `rgb` (0xRRGGBB) colours it; 0xFFFFFFFF keeps the default. With a colour, a dot's ring glows faintly in it too
// (the picker: each Spotify device has its own colour).
void ringShowLevel(uint8_t level, uint8_t of = 10, bool dot = false, uint32_t ms = 1500, uint32_t rgb = 0xFFFFFFFF);
// Spotify mode's answer to a command: a sweep (ok; green, or `rgb`) or a red blink, for about a second.
void ringFlash(bool ok, uint32_t rgb = 0xFFFFFFFF);
// Two red LEDs at the top while the hardware mute switch is on.
void ringSetMuted(bool muted);
// Call every loop; renders at most every 20 ms.
void ringUpdate();
