#include "sb_codec.h"

#include "sb_config.h"

#if SB_HAS_AIC3204

#include <Wire.h>

namespace {

constexpr uint8_t kAic3204Address = 0x18;  // TI datasheet: I2C address 0011000.

struct RegWrite {
  uint8_t reg;
  uint8_t value;
};

// Register sequence from the ESPHome aic3204 driver (AIC3204::setup).
// scripts/check-aic3204.mjs compares both tables against that source.
// The clocks assume MCLK = 24.576 MHz from the XMOS and 48 kHz audio.
// AIC3204_STAGE1_BEGIN
constexpr RegWrite kStage1[] = {
    {0x00, 0x00},  // Select page 0
    {0x01, 0x01},  // Software reset
    {0x0B, 0x82},  // NDAC on, divide by 2
    {0x0C, 0x82},  // MDAC on, divide by 2
    {0x0E, 0x80},  // DOSR = 128
    {0x1B, 0x30},  // I2S, 32 bits
    {0x38, 0x02},  // SCLK/MFP3 is the audio data input
    {0x1F, 0x01},  // Audio interface 4
    {0x20, 0x01},  // Audio interface 5
    {0x3C, 0x01},  // DAC processing block PRB_P1
    {0x00, 0x01},  // Select page 1
    {0x02, 0x09},  // Enable the internal AVDD LDO
    {0x01, 0x08},  // Disable the crude AVDD supply
    {0x02, 0x01},  // Master analog power on
    {0x0A, 0x40},  // Common mode 0.75 V
    {0x03, 0x00},  // Left DAC PowerTune, class AB
    {0x04, 0x00},  // Right DAC PowerTune, class AB
    {0x7B, 0x01},  // Reference charge time 40 ms
    {0x14, 0x25},  // Headphone soft-step start
    {0x0C, 0x08},  // Left DAC to HPL
    {0x0D, 0x08},  // Right DAC to HPR
    {0x0E, 0x08},  // Left DAC to LOL
    {0x0F, 0x08},  // Right DAC to LOR
    {0x10, 0x3E},  // HPL unmute, -2 dB
    {0x11, 0x3E},  // HPR unmute, -2 dB
    {0x12, 0x00},  // LOL unmute, 0 dB
    {0x13, 0x00},  // LOR unmute, 0 dB
    {0x09, 0x3C},  // Power on HPL, HPR, LOL, LOR
};
// AIC3204_STAGE1_END

// The driver waits 2.5 s for the soft-step before it powers the DAC.
// AIC3204_STAGE2_BEGIN
constexpr RegWrite kStage2[] = {
    {0x00, 0x00},  // Select page 0
    {0x3F, 0xD4},  // Power on left and right DAC
};
// AIC3204_STAGE2_END

int readReg(uint8_t reg) {
  Wire.beginTransmission(kAic3204Address);
  Wire.write(reg);
  if (Wire.endTransmission(false) != 0) return -1;
  return Wire.requestFrom(kAic3204Address, (uint8_t)1) == 1 ? Wire.read() : -1;
}

bool writeReg(uint8_t reg, uint8_t value) {
  Wire.beginTransmission(kAic3204Address);
  Wire.write(reg);
  Wire.write(value);
  return Wire.endTransmission() == 0;
}

template <size_t N>
bool writeAll(const RegWrite (&table)[N]) {
  for (const RegWrite &w : table) {
    if (!writeReg(w.reg, w.value)) {
      Serial.printf("AIC3204 write failed: reg 0x%02X.\n", w.reg);
      return false;
    }
  }
  return true;
}

}  // namespace

bool sbCodecBegin() {
  pinMode(SB_PIN_MUTE, INPUT);

  // Keep the amplifier off during DAC setup to prevent a pop.
  pinMode(SB_PIN_AMP_ENABLE, OUTPUT);
  digitalWrite(SB_PIN_AMP_ENABLE, LOW);

  // Reset the XMOS. It starts from its own flash and then drives the I2S
  // clocks. This firmware does not update the XMOS image.
  pinMode(SB_PIN_XMOS_RESET, OUTPUT);
  digitalWrite(SB_PIN_XMOS_RESET, HIGH);
  delay(1);
  digitalWrite(SB_PIN_XMOS_RESET, LOW);

  Wire.begin(SB_PIN_I2C_SDA, SB_PIN_I2C_SCL);

  // eve (bench 2026-09-26): wait for the XMOS to boot before touching the
  // DAC, as ESPHome's voice_kit does (3 s). Configured straight after the
  // reset, the DAC came up with NDAC/MDAC off, 16-bit I2S and page 1 at its
  // defaults: the XMOS boot wiped the writes, and the speaker stayed silent.
  delay(3000);

  // Read the clock dividers and output power back, and redo the whole
  // sequence if the DAC lost them.
  for (int attempt = 1; attempt <= 3; attempt++) {
    if (!writeAll(kStage1)) {
      return false;
    }
    delay(2500);
    if (!writeAll(kStage2)) {
      return false;
    }
    int ndac = readReg(0x0B), iface = readReg(0x1B);
    writeReg(0x00, 0x01);
    int outPower = readReg(0x09);
    writeReg(0x00, 0x00);
    if (ndac == 0x82 && iface == 0x30 && outPower == 0x3C) {
      break;
    }
    Serial.printf("AIC3204 lost its setup (NDAC 0x%02X, iface 0x%02X, out 0x%02X), attempt %d; redoing it.\n",
                  ndac & 0xFF, iface & 0xFF, outPower & 0xFF, attempt);
    if (attempt == 3) {
      return false;
    }
  }

  int8_t volume = static_cast<int8_t>(constrain(SB_CODEC_VOLUME_STEPS, -127, 48));
  bool ok = writeReg(0x41, static_cast<uint8_t>(volume))  // Left DAC volume
            && writeReg(0x42, static_cast<uint8_t>(volume))  // Right DAC volume
            && writeReg(0x40, 0x00);                         // Unmute both DAC channels
  if (!ok) {
    Serial.println("AIC3204 volume setup failed.");
    return false;
  }

  digitalWrite(SB_PIN_AMP_ENABLE, HIGH);
  Serial.println("AIC3204 ready. Speaker amplifier on.");
  return true;
}

bool sbCodecMicMuted() {
  return digitalRead(SB_PIN_MUTE) == HIGH;
}

#else

bool sbCodecBegin() {
  return true;
}

bool sbCodecMicMuted() {
  return false;
}

#endif
