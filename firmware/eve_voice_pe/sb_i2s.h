#pragma once

#include <Arduino.h>

// I2S access for the microphone and the speaker.
//
// DevKit: the ESP32 is the clock master, through the Arduino ESP_I2S library.
// Voice PE: the XMOS chip is the clock master, so the ESP32-S3 is the slave.
// ESP_I2S supports the master role only, so that build uses the ESP-IDF
// i2s_std driver.

bool sbI2sMicBegin();
// Reads raw 32-bit samples. The samples are interleaved when
// SB_MIC_I2S_CHANNELS is more than 1. Returns the number of bytes read.
size_t sbI2sMicRead(int32_t *buffer, size_t bytes);

bool sbI2sSpeakerBegin();
// Writes 16-bit mono samples to both speaker channels. Blocks until done.
void sbI2sSpeakerWriteMono(const int16_t *samples, size_t count);

// eve diagnostic (Voice PE build only): speaker writes that failed, and bytes written.
void sbI2sSpeakerStats(uint32_t *errors, uint32_t *bytes);
