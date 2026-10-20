//! The microphone, as `ears.py` and `desk_call.py` use sounddevice: list the devices, open a mono int16
//! input stream at a fixed rate and block size, start, stop, close, and refresh the device table.
//!
//! `AudioBackend` is the seam (the Python tests replace `sounddevice` in `sys.modules`); `CpalBackend` is
//! the real one, CoreAudio through cpal.
//!
//! PortAudio converts whatever the device delivers to the requested 24 kHz mono int16 itself. cpal does
//! not, and asking CoreAudio for 24 kHz would change the device's nominal rate under every other app, so
//! the stream opens at the device's own default format and this module converts: the first channel,
//! resampled with sherpa-onnx's linear resampler, scaled to int16 the way PortAudio's undithered
//! float-to-int16 converter does (`x * 32767`, truncated, clipped), and cut into blocks of exactly the
//! requested size, as sounddevice's `blocksize` guarantees.

use std::sync::mpsc;
use std::thread;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample};
use parking_lot::Mutex;

/// One block of mono int16 samples, called on the audio thread.
pub type BlockCallback = Box<dyn FnMut(&[i16]) + Send>;
/// A stream status or error (sounddevice's `status` argument), called on the audio thread.
pub type StatusCallback = Box<dyn FnMut(String) + Send>;

/// One entry of `sd.query_devices()`.
#[derive(Clone, Debug, PartialEq)]
pub struct DeviceInfo {
    pub name: String,
    pub max_input_channels: u32,
}

/// What `sd.InputStream(samplerate=..., channels=1, dtype="int16", blocksize=..., device=...)` asks for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InputSpec {
    pub sample_rate: u32,
    pub block_frames: usize,
    /// An index into `query_devices()`; None is the default input.
    pub device: Option<usize>,
}

/// An opened input stream: `start()`, `stop()`, `close()`.
pub trait InputStream: Send {
    fn start(&mut self) -> Result<(), String>;
    fn stop(&mut self) -> Result<(), String>;
    fn close(&mut self) -> Result<(), String>;
}

/// sounddevice, as far as eve uses it.
pub trait AudioBackend: Send + Sync {
    /// `import sounddevice`: Err(reason) when there is no audio input to talk to at all.
    fn available(&self) -> Result<(), String>;
    /// `sd.query_devices()`.
    fn query_devices(&self) -> Result<Vec<DeviceInfo>, String>;
    /// `sd.query_devices(device)["name"]`, None being `sd.default.device[0]`.
    fn device_name(&self, device: Option<usize>) -> Result<String, String>;
    /// `sd.InputStream(...)`: opened, not started.
    fn make_input(
        &self,
        spec: InputSpec,
        on_block: BlockCallback,
        on_status: StatusCallback,
    ) -> Result<Box<dyn InputStream>, String>;
    /// `sd._terminate(); sd._initialize()`: rebuild the device table.
    fn refresh(&self) -> Result<(), String>;
}

// ---- conversion ---------------------------------------------------------------------

/// Device frames in, exact `block_frames` blocks of 24 kHz (or whatever was asked) int16 out.
struct Converter {
    channels: usize,
    in_rate: u32,
    out_rate: u32,
    resampler: Option<sherpa_onnx::LinearResampler>,
    block_frames: usize,
    pending: Vec<i16>,
    on_block: BlockCallback,
}

impl Converter {
    fn new(channels: usize, in_rate: u32, spec: &InputSpec, on_block: BlockCallback) -> Result<Self, String> {
        let resampler = if in_rate == spec.sample_rate {
            None
        } else {
            Some(
                sherpa_onnx::LinearResampler::create(in_rate as i32, spec.sample_rate as i32)
                    .ok_or_else(|| format!("no resampler from {in_rate} Hz to {} Hz", spec.sample_rate))?,
            )
        };
        Ok(Converter {
            channels: channels.max(1),
            in_rate,
            out_rate: spec.sample_rate,
            resampler,
            block_frames: spec.block_frames.max(1),
            pending: Vec::new(),
            on_block,
        })
    }

    fn emit(&mut self) {
        while self.pending.len() >= self.block_frames {
            let rest = self.pending.split_off(self.block_frames);
            let block = std::mem::replace(&mut self.pending, rest);
            (self.on_block)(&block);
        }
    }

    fn push_i16_native(&mut self, data: &[i16]) {
        self.pending.extend(data.iter().step_by(self.channels));
        self.emit();
    }

    fn push<T>(&mut self, data: &[T])
    where
        T: Sample,
        f32: FromSample<T>,
    {
        let mono: Vec<f32> = data.iter().step_by(self.channels).map(|s| f32::from_sample(*s)).collect();
        let samples = match &self.resampler {
            Some(r) => r.resample(&mono, false),
            None => mono,
        };
        self.pending.extend(samples.iter().map(|&x| float_to_int16(x)));
        self.emit();
    }
}

/// PortAudio's `Float32_To_Int16_Clip`: scale by 32767, truncate, clip.
pub fn float_to_int16(x: f32) -> i16 {
    let v = (x * 32767.0) as i64;
    v.clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16
}

// ---- cpal ---------------------------------------------------------------------------

/// CoreAudio (or the platform's default host) through cpal. The device table is listed once and again on
/// `refresh()`, like PortAudio's, so an index from `query_devices()` names the same device until then.
pub struct CpalBackend {
    devices: Mutex<Vec<cpal::Device>>,
}

impl Default for CpalBackend {
    fn default() -> Self {
        Self::new()
    }
}

fn list_devices() -> Vec<cpal::Device> {
    cpal::default_host().devices().map(|d| d.collect()).unwrap_or_default()
}

fn max_input_channels(d: &cpal::Device) -> u32 {
    d.supported_input_configs()
        .map(|it| it.map(|c| u32::from(c.channels())).max().unwrap_or(0))
        .unwrap_or(0)
}

fn device_label(d: &cpal::Device) -> Result<String, String> {
    d.description().map(|desc| desc.name().to_string()).map_err(|e| e.to_string())
}

impl CpalBackend {
    pub fn new() -> Self {
        CpalBackend { devices: Mutex::new(list_devices()) }
    }

    fn device(&self, device: Option<usize>) -> Result<cpal::Device, String> {
        match device {
            Some(i) => self.devices.lock().get(i).cloned().ok_or_else(|| format!("Error querying device {i}")),
            None => cpal::default_host()
                .default_input_device()
                .ok_or_else(|| "Error querying device -1".to_string()),
        }
    }
}

type Reply = mpsc::Sender<Result<(), String>>;

enum Command {
    Start,
    Stop,
}

/// The cpal stream lives on its own thread (it is not `Send` on every platform); this handle drives it.
struct CpalStream {
    commands: Option<mpsc::Sender<(Command, Reply)>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl CpalStream {
    fn send(&self, c: Command) -> Result<(), String> {
        let tx = self.commands.as_ref().ok_or_else(|| "stream is closed".to_string())?;
        let (reply, rx) = mpsc::channel();
        tx.send((c, reply)).map_err(|_| "stream thread is gone".to_string())?;
        rx.recv().map_err(|_| "stream thread is gone".to_string())?
    }
}

impl InputStream for CpalStream {
    fn start(&mut self) -> Result<(), String> {
        self.send(Command::Start)
    }

    fn stop(&mut self) -> Result<(), String> {
        self.send(Command::Stop)
    }

    fn close(&mut self) -> Result<(), String> {
        self.commands = None;
        if let Some(t) = self.thread.take() {
            t.join().map_err(|_| "stream thread panicked".to_string())?;
        }
        Ok(())
    }
}

impl Drop for CpalStream {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

fn build_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut conv: Converter,
    mut on_status: StatusCallback,
) -> Result<cpal::Stream, String>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let native_i16 = T::FORMAT == SampleFormat::I16 && conv.in_rate == conv.out_rate;
    device
        .build_input_stream::<T, _, _>(
            config,
            move |data: &[T], _info| {
                if native_i16 {
                    // Already what was asked for: pass the samples through untouched.
                    // SAFETY: T is i16 here (checked through T::FORMAT above).
                    let raw = unsafe { std::slice::from_raw_parts(data.as_ptr() as *const i16, data.len()) };
                    conv.push_i16_native(raw);
                } else {
                    conv.push(data);
                }
            },
            move |e| on_status(e.to_string()),
            None,
        )
        .map_err(|e| e.to_string())
}

fn open_cpal(
    device: cpal::Device,
    spec: InputSpec,
    on_block: BlockCallback,
    on_status: StatusCallback,
) -> Result<cpal::Stream, String> {
    let supported = device.default_input_config().map_err(|e| e.to_string())?;
    let config = supported.config();
    let conv = Converter::new(usize::from(config.channels), config.sample_rate, &spec, on_block)?;
    match supported.sample_format() {
        SampleFormat::F32 => build_stream::<f32>(&device, config, conv, on_status),
        SampleFormat::F64 => build_stream::<f64>(&device, config, conv, on_status),
        SampleFormat::I8 => build_stream::<i8>(&device, config, conv, on_status),
        SampleFormat::I16 => build_stream::<i16>(&device, config, conv, on_status),
        SampleFormat::I32 => build_stream::<i32>(&device, config, conv, on_status),
        SampleFormat::U8 => build_stream::<u8>(&device, config, conv, on_status),
        SampleFormat::U16 => build_stream::<u16>(&device, config, conv, on_status),
        other => Err(format!("Invalid sample format {other}")),
    }
}

impl AudioBackend for CpalBackend {
    fn available(&self) -> Result<(), String> {
        let host = cpal::default_host();
        if cpal::available_hosts().contains(&host.id()) {
            Ok(())
        } else {
            Err(format!("audio host {} is not available", host.id().name()))
        }
    }

    fn query_devices(&self) -> Result<Vec<DeviceInfo>, String> {
        Ok(self
            .devices
            .lock()
            .iter()
            .map(|d| DeviceInfo {
                name: device_label(d).unwrap_or_default(),
                max_input_channels: max_input_channels(d),
            })
            .collect())
    }

    fn device_name(&self, device: Option<usize>) -> Result<String, String> {
        device_label(&self.device(device)?)
    }

    fn make_input(
        &self,
        spec: InputSpec,
        on_block: BlockCallback,
        on_status: StatusCallback,
    ) -> Result<Box<dyn InputStream>, String> {
        let device = self.device(spec.device)?;
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let (cmd_tx, cmd_rx) = mpsc::channel::<(Command, mpsc::Sender<Result<(), String>>)>();
        let thread = thread::Builder::new()
            .name("eve-mic".into())
            .spawn(move || {
                let stream = match open_cpal(device, spec, on_block, on_status) {
                    Ok(s) => {
                        // cpal may start capturing as soon as the stream is built; sounddevice's stream
                        // is silent until start().
                        let _ = s.pause();
                        let _ = ready_tx.send(Ok(()));
                        s
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                // Runs until the handle is closed or dropped; the stream is dropped (closed) here.
                while let Ok((cmd, reply)) = cmd_rx.recv() {
                    let r = match cmd {
                        Command::Start => stream.play().map_err(|e| e.to_string()),
                        Command::Stop => stream.pause().map_err(|e| e.to_string()),
                    };
                    let _ = reply.send(r);
                }
            })
            .map_err(|e| e.to_string())?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Box::new(CpalStream { commands: Some(cmd_tx), thread: Some(thread) })),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = thread.join();
                Err("stream thread is gone".into())
            }
        }
    }

    fn refresh(&self) -> Result<(), String> {
        *self.devices.lock() = list_devices();
        Ok(())
    }
}

#[cfg(test)]
pub(crate) fn converter_for_tests(
    channels: usize,
    in_rate: u32,
    spec: &InputSpec,
    on_block: BlockCallback,
) -> Result<impl FnMut(&[f32]), String> {
    let mut c = Converter::new(channels, in_rate, spec, on_block)?;
    Ok(move |data: &[f32]| c.push(data))
}

#[cfg(test)]
pub(crate) fn converter_i16_for_tests(
    channels: usize,
    spec: &InputSpec,
    on_block: BlockCallback,
) -> Result<impl FnMut(&[i16]), String> {
    let mut c = Converter::new(channels, spec.sample_rate, spec, on_block)?;
    Ok(move |data: &[i16]| c.push_i16_native(data))
}
