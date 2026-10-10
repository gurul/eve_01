#include "sb_i2s.h"

#include "sb_config.h"
#include "pcm_convert.h"

#if SB_I2S_SLAVE

#include <driver/i2s_std.h>

namespace {

i2s_chan_handle_t micChannel = nullptr;
i2s_chan_handle_t speakerChannel = nullptr;
uint32_t speakerWriteErrors = 0;   // eve diagnostic: writes that timed out or failed
uint32_t speakerBytesWritten = 0;

i2s_std_config_t slaveConfig(uint32_t sampleRate, i2s_data_bit_width_t bits, i2s_slot_mode_t slotMode) {
  i2s_std_config_t cfg = {};
  cfg.clk_cfg = I2S_STD_CLK_DEFAULT_CONFIG(sampleRate);
  cfg.slot_cfg = I2S_STD_PHILIPS_SLOT_DEFAULT_CONFIG(bits, slotMode);
  cfg.gpio_cfg.mclk = I2S_GPIO_UNUSED;
  cfg.gpio_cfg.bclk = I2S_GPIO_UNUSED;
  cfg.gpio_cfg.ws = I2S_GPIO_UNUSED;
  cfg.gpio_cfg.dout = I2S_GPIO_UNUSED;
  cfg.gpio_cfg.din = I2S_GPIO_UNUSED;
  return cfg;
}

}  // namespace

bool sbI2sMicBegin() {
  i2s_chan_config_t chanCfg = I2S_CHANNEL_DEFAULT_CONFIG(I2S_NUM_0, I2S_ROLE_SLAVE);
  if (i2s_new_channel(&chanCfg, nullptr, &micChannel) != ESP_OK) {
    return false;
  }

  i2s_std_config_t cfg = slaveConfig(SB_AUDIO_SAMPLE_RATE, I2S_DATA_BIT_WIDTH_32BIT,
                                     SB_MIC_I2S_CHANNELS > 1 ? I2S_SLOT_MODE_STEREO : I2S_SLOT_MODE_MONO);
  cfg.gpio_cfg.bclk = static_cast<gpio_num_t>(SB_PIN_MIC_SCK);
  cfg.gpio_cfg.ws = static_cast<gpio_num_t>(SB_PIN_MIC_WS);
  cfg.gpio_cfg.din = static_cast<gpio_num_t>(SB_PIN_MIC_SD);
  if (i2s_channel_init_std_mode(micChannel, &cfg) != ESP_OK) {
    return false;
  }
  return i2s_channel_enable(micChannel) == ESP_OK;
}

size_t sbI2sMicRead(int32_t *buffer, size_t bytes) {
  if (!micChannel) {
    return 0;
  }
  size_t bytesRead = 0;
  // A timeout stops a missing XMOS clock from blocking the capture task.
  i2s_channel_read(micChannel, buffer, bytes, &bytesRead, pdMS_TO_TICKS(200));
  return bytesRead;
}

bool sbI2sSpeakerBegin() {
  i2s_chan_config_t chanCfg = I2S_CHANNEL_DEFAULT_CONFIG(I2S_NUM_1, I2S_ROLE_SLAVE);
  chanCfg.auto_clear = true;
  if (i2s_new_channel(&chanCfg, &speakerChannel, nullptr) != ESP_OK) {
    return false;
  }

  i2s_std_config_t cfg = slaveConfig(SB_SPK_SAMPLE_RATE, I2S_DATA_BIT_WIDTH_32BIT, I2S_SLOT_MODE_STEREO);
  cfg.gpio_cfg.bclk = static_cast<gpio_num_t>(SB_PIN_SPK_BCLK);
  cfg.gpio_cfg.ws = static_cast<gpio_num_t>(SB_PIN_SPK_LRC);
  cfg.gpio_cfg.dout = static_cast<gpio_num_t>(SB_PIN_SPK_DIN);
  if (i2s_channel_init_std_mode(speakerChannel, &cfg) != ESP_OK) {
    return false;
  }
  return i2s_channel_enable(speakerChannel) == ESP_OK;
}

void sbI2sSpeakerWriteMono(const int16_t *samples, size_t count) {
  if (!speakerChannel) {
    return;
  }
  int32_t frames[128 * 2];
  while (count > 0) {
    size_t n = count > 128 ? 128 : count;
    for (size_t i = 0; i < n; i++) {
      int32_t wide = sbPcm16To32(samples[i]);
      frames[i * 2] = wide;
      frames[i * 2 + 1] = wide;
    }
    size_t written = 0;
    if (i2s_channel_write(speakerChannel, frames, n * 2 * sizeof(int32_t), &written, pdMS_TO_TICKS(500)) != ESP_OK) {
      speakerWriteErrors++;
      return;  // No clock from the XMOS. Drop the sound, do not block.
    }
    speakerBytesWritten += written;
    samples += n;
    count -= n;
  }
}

void sbI2sSpeakerStats(uint32_t *errors, uint32_t *bytes) {
  *errors = speakerWriteErrors;
  *bytes = speakerBytesWritten;
}

#else  // The ESP32 is the I2S clock master.

#include <ESP_I2S.h>

namespace {

I2SClass micI2S(I2S_NUM_0);
I2SClass speakerI2S(I2S_NUM_1);
bool speakerReady = false;

}  // namespace

bool sbI2sMicBegin() {
  micI2S.setPins(SB_PIN_MIC_SCK, SB_PIN_MIC_WS, -1, SB_PIN_MIC_SD);
  return micI2S.begin(I2S_MODE_STD, SB_AUDIO_SAMPLE_RATE, I2S_DATA_BIT_WIDTH_32BIT, I2S_SLOT_MODE_MONO,
                      SB_MIC_SLOT_MASK);
}

size_t sbI2sMicRead(int32_t *buffer, size_t bytes) {
  return micI2S.readBytes(reinterpret_cast<char *>(buffer), bytes);
}

bool sbI2sSpeakerBegin() {
  speakerI2S.setPins(SB_PIN_SPK_BCLK, SB_PIN_SPK_LRC, SB_PIN_SPK_DIN);
  speakerReady = speakerI2S.begin(I2S_MODE_STD, SB_SPK_SAMPLE_RATE, I2S_DATA_BIT_WIDTH_16BIT, I2S_SLOT_MODE_STEREO);
  return speakerReady;
}

void sbI2sSpeakerWriteMono(const int16_t *samples, size_t count) {
  if (!speakerReady) {
    return;
  }
  int16_t frames[128 * 2];
  while (count > 0) {
    size_t n = count > 128 ? 128 : count;
    for (size_t i = 0; i < n; i++) {
      frames[i * 2] = samples[i];
      frames[i * 2 + 1] = samples[i];
    }
    speakerI2S.write(reinterpret_cast<const uint8_t *>(frames), n * 2 * sizeof(int16_t));
    samples += n;
    count -= n;
  }
}

#endif
