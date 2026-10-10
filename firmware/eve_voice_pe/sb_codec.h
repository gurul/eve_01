#pragma once

#include <Arduino.h>

// Audio hardware setup that must run before I2S starts.
// Voice PE: resets the XMOS, configures the AIC3204 DAC over I2C, and
// enables the speaker amplifier. DevKit: does nothing and returns true.
bool sbCodecBegin();

// True when the hardware mute switch cut the microphone power.
// Always false on a board without a mute switch.
bool sbCodecMicMuted();
