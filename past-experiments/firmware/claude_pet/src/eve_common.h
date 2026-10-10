#pragma once
#include <stdint.h>

// Shared constants and helpers for eve species files.
// Each species file (src/eves/<name>.cpp) includes this header
// and defines its 7 state functions.

// Geometry — shared layout for all species
extern const int EVE_X_CENTER;
extern const int EVE_CANVAS_W;
extern const int EVE_Y_BASE;
extern const int EVE_Y_OVERLAY;
extern const int EVE_CHAR_W;
extern const int EVE_CHAR_H;

// Common colors species can use freely
extern const uint16_t EVE_BG;
extern const uint16_t EVE_HEART;
extern const uint16_t EVE_DIM;
extern const uint16_t EVE_YEL;
extern const uint16_t EVE_WHITE;
extern const uint16_t EVE_CYAN;
extern const uint16_t EVE_GREEN;
extern const uint16_t EVE_PURPLE;
extern const uint16_t EVE_RED;
extern const uint16_t EVE_BLUE;

// Print one line centered around EVE_X_CENTER, optionally x-offset.
void evePrintLine(const char* line, int yPx, uint16_t color, int xOff = 0);

// Print N-line sprite block. yOffset is added to EVE_Y_BASE for the top row.
void evePrintSprite(const char* const* lines, uint8_t nLines, int yOffset, uint16_t color, int xOff = 0);

// Set sprite text color directly + cursor (for ad-hoc particle drawing).
void eveSetCursor(int x, int y);
void eveSetColor(uint16_t fg);
void evePrint(const char* s);

// Vector fill in 1x eve coordinates: x is re-centered around
// EVE_X_CENTER and everything is multiplied by the internal scale,
// mirroring eveSetCursor, and routed through the current render target.
