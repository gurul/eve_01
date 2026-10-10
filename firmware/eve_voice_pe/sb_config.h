#pragma once

// Home Assistant Voice Preview Edition (Nabu Casa NC-VK-9727).
// ESP32-S3, 16 MB flash, 8 MB octal PSRAM.
//
// sb_i2s.* and sb_codec.* come unchanged from homeboxV0
// (firmware/storeybox_esp32, commit 9c8a4f8), whose scripts/check-pins.mjs and
// scripts/check-aic3204.mjs compare them against the upstream ESPHome
// firmware. This header supplies the defines those two files read, with the
// values from homeboxV0's sb_board_voice_pe.h.
//
// The XMOS XU316 is the I2S clock master on both audio buses. The ESP32-S3
// is the slave: it never drives BCLK or LRCLK.

// Internal I2C bus: AIC3204 DAC (0x18) and XMOS control (0x42).
#define SB_PIN_I2C_SDA 5
#define SB_PIN_I2C_SCL 6

// Microphone bus, from the XMOS. Unused: eve listens on the Mac.
#define SB_PIN_MIC_SCK 13
#define SB_PIN_MIC_WS 14
#define SB_PIN_MIC_SD 15
#define SB_AUDIO_SAMPLE_RATE 16000
#define SB_MIC_I2S_CHANNELS 2

// Speaker bus, to the AIC3204 through the XMOS. 48 kHz, 32-bit, stereo.
#define SB_PIN_SPK_BCLK 8
#define SB_PIN_SPK_LRC 7
#define SB_PIN_SPK_DIN 10
#define SB_SPK_SAMPLE_RATE 48000

#define SB_PIN_XMOS_RESET 4
#define SB_PIN_AMP_ENABLE 47

// Hardware mute switch. HIGH means the switch cut the microphone power.
#define SB_PIN_MUTE 3

#define SB_PIN_LED_RING 21
#define SB_PIN_LED_POWER 45
#define SB_LED_RING_COUNT 12

// Center button, active low. GPIO 0 is also the boot strap pin: holding it
// while plugging in USB starts the ROM bootloader.
#define SB_PIN_BUTTON 0

// Rotary dial, quadrature.
#define SB_PIN_DIAL_A 16
#define SB_PIN_DIAL_B 18

#define SB_I2S_SLAVE 1
#define SB_HAS_AIC3204 1
#define SB_CODEC_VOLUME_STEPS 0
