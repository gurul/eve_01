#pragma once
// ASCII cat for the e-ink pet — poses adapted from firmware/claude_pet/src/
// eves/cat.cpp (MIT, Anthropic PBC). Two frames per state; the renderer
// alternates them on the minute tick, because an e-paper panel is not a place
// for 12fps animation. Rows are drawn monospace, 12 columns × 5 rows.

#define ART_ROWS 5
#define ART_COLS 12

// Index: [PersonaState][frame][row] — PersonaState order matches main sketch:
// 0=sleep 1=idle 2=busy 3=attention 4=celebrate 5=dizzy 6=heart
static const char* const PET_ART[7][2][ART_ROWS] = {
  { // sleep — loaf, z-stream
    { "        z   ",
      "      Z     ",
      "   .-..-.   ",
      "  ( -.- )   ",
      " `~------'~ " },
    { "      z     ",
      "        Z   ",
      "   .-..-.   ",
      "  ( u.u )   ",
      " `~------'  " },
  },
  { // idle — sitting, blink
    { "            ",
      "   /\\_/\\    ",
      "  ( o   o ) ",
      "  (  w   )  ",
      "  (\")_(\")   " },
    { "            ",
      "   /\\_/\\    ",
      "  ( -   - ) ",
      "  (  w   )  ",
      "  (\")_(\")   " },
  },
  { // busy — paw batting, wide-eyed stare
    { "      .     ",
      "   /\\_/\\    ",
      "  ( o   o ) ",
      "  (  w   )/ ",
      "  (\")_(\")   " },
    { "  ..        ",
      "   /\\_/\\    ",
      "  ( O   O ) ",
      "  (  w   )_ ",
      "  (\")_(\")   " },
  },
  { // attention — ears up, pupils blown
    { "     !!     ",
      "   /^_^\\    ",
      "  ( O   O ) ",
      "  (  v   )  ",
      "  (\")_(\")   " },
    { "      !     ",
      "   /^_^\\    ",
      "  (O    O ) ",
      "  (  v   )  ",
      "  (\")_(\")   " },
  },
  { // celebrate — stars
    { " *   *    * ",
      "   /\\_/\\    ",
      "  ( ^   ^ ) ",
      " *(  w   )* ",
      "  (\")_(\")   " },
    { "   *    *   ",
      "   /\\_/\\    ",
      "  ( ^   ^ ) ",
      "  (  w   )* ",
      " *(\")_(\")   " },
  },
  { // dizzy — spirals
    { "    @  @    ",
      "   /\\_/\\    ",
      "  ( x   x ) ",
      "  (  ~   )  ",
      "  (\")_(\")   " },
    { "   @    @   ",
      "   /\\_/\\    ",
      "  ( +   + ) ",
      "  (  ~   )  ",
      "  (\")_(\")   " },
  },
  { // heart
    { "    <3      ",
      "   /\\_/\\    ",
      "  ( ^   ^ ) ",
      "  (  3   )  ",
      "  (\")_(\")   " },
    { "      <3    ",
      "   /\\_/\\    ",
      "  ( ^   ^ ) ",
      "  (  w   )  ",
      "  (\")_(\")   " },
  },
};
