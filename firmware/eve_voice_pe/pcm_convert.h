#pragma once
#include <stdint.h>

// Signed 16-bit PCM in the most significant half of a 32-bit I2S slot.
// Multiplication is defined for negative samples; a signed left shift is not in C++17.
constexpr int32_t sbPcm16To32(int16_t sample) { return int32_t(sample) * 65536; }
