//! The Mac speaker, as `desk_call.Speaker` uses sounddevice: `sd.RawOutputStream(samplerate=24000, channels=1,
//! dtype="int16")`, started, written with blocking `write(pcm)`, then stopped and closed.
//!
//! PortAudio converts the 24 kHz mono int16 it is handed to whatever the device plays. cpal does not, and asking
//! CoreAudio for 24 kHz would change the device's nominal rate under every other app, so the stream opens at the
//! device's default format and this module converts: int16 to float the way PortAudio's `Int16_To_Float32` does
//! (`x / 32768`), resampled linearly to the device's rate, copied to every channel. A write blocks while more than
//! [`BUFFER_SECS`] of audio waits to be played, as a PortAudio blocking write does once its buffer is full.
//!
//! The cpal stream is not `Send` everywhere, so it is opened and used on the one thread that plays (the Speaker's
//! player thread), as the Python opens it inside `_run`.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use parking_lot::Mutex;

/// How much audio may wait in the output buffer before a write blocks.
pub const BUFFER_SECS: f64 = 0.1;

/// An opened, started output stream (`RawOutputStream` after `start()`).
pub trait RawOutput {
    /// `out.write(pcm)`: mono int16 at the rate it was opened with; blocks until it fits.
    fn write(&mut self, pcm: &[u8]) -> Result<(), String>;
    /// `out.stop(); out.close()`: what was written plays out first, as PortAudio's `Pa_StopStream` waits for it.
    fn stop_close(&mut self);
}

/// Opens the output on the calling thread (the player thread).
pub type OutputOpener = Arc<dyn Fn(u32) -> Result<Box<dyn RawOutput>, String> + Send + Sync>;

/// The default output device through cpal.
pub fn open_default_output(sample_rate: u32) -> Result<Box<dyn RawOutput>, String> {
    let device =
        cpal::default_host().default_output_device().ok_or_else(|| "Error querying device -1".to_string())?;
    let supported = device.default_output_config().map_err(|e| e.to_string())?;
    let config = supported.config();
    let buffer = Arc::new(Mutex::new(VecDeque::<f32>::new()));
    let stream = match supported.sample_format() {
        SampleFormat::F32 => build::<f32>(&device, &config, buffer.clone()),
        SampleFormat::F64 => build::<f64>(&device, &config, buffer.clone()),
        SampleFormat::I8 => build::<i8>(&device, &config, buffer.clone()),
        SampleFormat::I16 => build::<i16>(&device, &config, buffer.clone()),
        SampleFormat::I32 => build::<i32>(&device, &config, buffer.clone()),
        SampleFormat::U8 => build::<u8>(&device, &config, buffer.clone()),
        SampleFormat::U16 => build::<u16>(&device, &config, buffer.clone()),
        other => Err(format!("Invalid sample format {other}")),
    }?;
    stream.play().map_err(|e| e.to_string())?;
    let out_rate = config.sample_rate;
    Ok(Box::new(CpalOutput {
        stream: Some(stream),
        buffer,
        resampler: Resampler::new(sample_rate, out_rate),
        cap: ((f64::from(out_rate) * BUFFER_SECS) as usize).max(1),
        out_rate,
    }))
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    buffer: Arc<Mutex<VecDeque<f32>>>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    let channels = usize::from(config.channels).max(1);
    device
        .build_output_stream::<T, _, _>(
            *config,
            move |data: &mut [T], _info| {
                let mut buf = buffer.lock();
                for frame in data.chunks_mut(channels) {
                    let x = buf.pop_front().unwrap_or(0.0);
                    for s in frame.iter_mut() {
                        *s = T::from_sample(x);
                    }
                }
            },
            |e| tracing::debug!("desk call: speaker stream status {}", e),
            None,
        )
        .map_err(|e| e.to_string())
}

/// Linear interpolation from one rate to another, carried across writes.
pub struct Resampler {
    step: f64,
    t: f64,
    last: f32,
    same: bool,
}

impl Resampler {
    pub fn new(in_rate: u32, out_rate: u32) -> Self {
        Resampler {
            step: f64::from(in_rate) / f64::from(out_rate.max(1)),
            t: 0.0,
            last: 0.0,
            same: in_rate == out_rate,
        }
    }

    pub fn push(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if self.same {
            out.extend_from_slice(input);
            return;
        }
        for &s in input {
            while self.t < 1.0 {
                out.push(self.last + (s - self.last) * self.t as f32);
                self.t += self.step;
            }
            self.t -= 1.0;
            self.last = s;
        }
    }
}

/// PortAudio's `Int16_To_Float32`.
pub fn int16_to_float(x: i16) -> f32 {
    f32::from(x) * (1.0 / 32768.0)
}

struct CpalOutput {
    stream: Option<cpal::Stream>,
    buffer: Arc<Mutex<VecDeque<f32>>>,
    resampler: Resampler,
    cap: usize,
    out_rate: u32,
}

impl RawOutput for CpalOutput {
    fn write(&mut self, pcm: &[u8]) -> Result<(), String> {
        let mono: Vec<f32> =
            pcm.chunks_exact(2).map(|b| int16_to_float(i16::from_ne_bytes([b[0], b[1]]))).collect();
        let mut samples = Vec::with_capacity(mono.len() * 2);
        self.resampler.push(&mono, &mut samples);
        for piece in samples.chunks(self.cap) {
            loop {
                {
                    let mut buf = self.buffer.lock();
                    if buf.len() <= self.cap {
                        buf.extend(piece.iter().copied());
                        break;
                    }
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        Ok(())
    }

    fn stop_close(&mut self) {
        // Let what was written play out, bounded by how much can be waiting.
        let left = self.buffer.lock().len();
        let limit = Instant::now() + Duration::from_secs_f64(left as f64 / f64::from(self.out_rate.max(1)) + 0.5);
        while !self.buffer.lock().is_empty() && Instant::now() < limit {
            std::thread::sleep(Duration::from_millis(5));
        }
        if let Some(stream) = self.stream.take() {
            let _ = stream.pause();
        }
    }
}

impl Drop for CpalOutput {
    fn drop(&mut self) {
        if let Some(stream) = self.stream.take() {
            let _ = stream.pause();
        }
    }
}
