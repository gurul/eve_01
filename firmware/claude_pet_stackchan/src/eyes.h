#pragma once
// FluxGarage RoboEyes face for the StackChan port.
//
// The eyes render into a 1-bit palette canvas (EYES_W x EYES_H) that
// eyesTick() pushes into the main sprite at (0, EYES_Y). RoboEyes itself
// lives only in eyes.cpp: its header defines bare macros (DEFAULT, N, E, S,
// W, ON, OFF) that collide with everything, so it is included in exactly
// one translation unit and never from a header.
#include <stdint.h>
#include "persona.h"
#include "mood.h"

// Eye area: full width, everything above the transcript HUD (y 0..203), so
// RoboEyes' own centring puts the eye pair at the panel's visual centre.
// While a permission card is up (band from y=126) the eyes move to the N
// row (y 0..96) so they stay above the band — see eyesCardUp().
constexpr int EYES_W = 320;
constexpr int EYES_H = 204;
constexpr int EYES_Y = 0;
// Status word row, inside the bottom of the eye area (drawn after the push).
constexpr int EYES_STATUS_Y = 186;

// After spr.createSprite(). Allocates the canvas, begin(w,h,50), geometry.
void eyesBegin();

// Eye colour: random per boot (HSV hue 0..359, sat 0.6..1.0, val 0.75..1.0,
// so never black / near-black on the dark face). Set EYE_COLOUR_OVERRIDE to
// an 0xRRGGBB value for a fixed colour; -1 = random. Boot prints
// `[eyes] colour #RRGGBB`.
constexpr int32_t EYE_COLOUR_OVERRIDE = -1;
// Palette index 0 = face background. Call when the character palette
// changes (and once after eyesBegin).
void eyesSetBackground(uint16_t bgRgb565);
// The eye colour as RGB565, for the status word.
uint16_t eyesColor();

// Mood/animation for the pet state. Re-applies RoboEyes settings only when
// one of the inputs changes; safe to call every frame.
//   needsAttention: base state is attention (a session waits)
//   listening:      host dictation key or panel hold is live
//   hotPrompt:      the pending permission is destructive (sweat while waiting)
//   gazeSide:       -1 screen left, +1 screen right, 0 none — used when the
//                   head is centred so the eyes still glance at the toucher
//   explore:        {"cmd":"mode","explore":true} — the mood engine's expression
//                   (mood != nullptr): openness, smile/droop, curiosity,
//                   blink and saccade tempo, a flicker when startled
//   agent:          the host conversation phase (AgentState): listening wide,
//                   thinking with quick saccades, working = squint + fast
//                   saccades, asking wide, done = laugh, error = confused
void eyesSet(PersonaState s, bool needsAttention, bool listening, bool hotPrompt, int8_t gazeSide,
             bool explore = false, uint8_t agent = 0, const MoodExpr* mood = nullptr, uint8_t expressionKind = 0);

// Keep the eyes and the head agreeing: head yaw/pitch in degrees →
// setPosition band (W / NW / N / NE / E, DEFAULT when centred).
void eyesLookAt(int8_t yawDeg);
void eyesLookAt(int8_t yawDeg, int8_t pitchDeg);

// A permission card owns y >= 126: park the eyes on the N row while true,
// back to the centred row when it clears.
void eyesCardUp(bool up);

// RoboEyes update() (50 fps internal timer), sleep peek timer, then the
// canvas is pushed into the main sprite at (0, EYES_Y).
void eyesTick(uint32_t now);

// Hold the viewer-right eye as a curved closed line for 650 ms; caller debounces event id.
void eyesWink();
bool eyesWinkClosed();

// One short word for the status row: "zzz", "working...", "needs you!",
// "listening...", "done!", "<3", "@_@". Empty for idle-without-daemon.
const char* eyesStatusText(PersonaState s, bool listening, bool explore = false, uint8_t agent = 0,
                           const MoodExpr* mood = nullptr);
