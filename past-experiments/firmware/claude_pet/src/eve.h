#pragma once
#include <stdint.h>

// Multi-species ASCII eve renderer. Each species lives in its own
// src/eves/<name>.cpp file and exposes 7 state functions matching
// the PersonaState enum order: sleep, idle, busy, attention, celebrate,
// dizzy, heart.
void eveInit();
void eveTick(uint8_t personaState);
void eveInvalidate();
class TFT_eSPI;
void eveRenderTo(TFT_eSPI* tgt, uint8_t personaState);
void eveSetSpecies(const char* name);
void eveSetSpeciesIdx(uint8_t idx);
void eveSetPeek(bool peek);
uint8_t eveSpeciesIdx();
uint8_t eveSpeciesCount();
const char* eveSpeciesName();

// Per-species state function: takes the global tickCount and renders
// the eve + any overlays for the current state into the shared sprite.
typedef void (*StateFn)(uint32_t t);

struct Species {
  const char* name;
  uint16_t bodyColor;
  StateFn states[7];   // index by PersonaState (0=sleep .. 6=heart)
};
