//! Opus, the codec of the peer's audio both ways: libopus, opened at run time the first time an offer comes.
//!
//! aiortc carried its own Opus (a cffi binding of libopus) and PyAV's resampler, both native libraries loaded on the
//! first offer: when they did not load, every offer was refused and the page chatted over HTTPS. webrtc-rs packetizes
//! RTP but has no codec, so this opens the system's libopus (Homebrew's on this Mac) the same way: `dlopen` on the
//! first offer, and a Mac without it refuses offers instead of failing to start or to link.
//!
//! Only the six calls the voice needs: an encoder and a decoder at 24 kHz mono, the call's own format. An Opus packet
//! does not depend on the rate it was encoded at, so eve's 24 kHz mono voice plays in a browser's 48 kHz stereo
//! decoder, and the page's 48 kHz stereo microphone decodes straight to 24 kHz mono (libopus averages the two
//! channels), which is what aiortc's decoder and `av.AudioResampler(format="s16", layout="mono", rate=24000)` gave.

use std::ffi::{c_char, c_int, c_void, CString};
use std::sync::OnceLock;

use parking_lot::Mutex;

/// `OPUS_APPLICATION_VOIP`, as aiortc's encoder (aiortc/codecs/opus.py).
pub const APPLICATION_VOIP: c_int = 2048;
/// The longest Opus packet is 120 ms: at 24 kHz, this many samples per channel.
pub const MAX_FRAME_SAMPLES: usize = 2880;
/// An encoded 20 ms frame never comes near this (libopus's own recommendation is 4000).
pub const MAX_PACKET_BYTES: usize = 4000;

/// Where libopus is looked for, in order: the loader's own search first, then Homebrew's prefixes (Apple silicon,
/// then Intel), then the Linux soname.
pub const LIBRARY_PATHS: &[&str] = &[
    "libopus.0.dylib",
    "/opt/homebrew/lib/libopus.0.dylib",
    "/opt/homebrew/opt/opus/lib/libopus.0.dylib",
    "/usr/local/lib/libopus.0.dylib",
    "/usr/local/opt/opus/lib/libopus.0.dylib",
    "libopus.so.0",
];

/// libopus did not load, or one of its calls failed (the code is libopus's own, `OPUS_*`).
#[derive(Debug, Clone, thiserror::Error)]
pub enum OpusError {
    /// No library on [`LIBRARY_PATHS`], or one without the calls (Python's OSError from a native library).
    #[error("libopus does not load: {0}")]
    Load(String),
    /// A call returned an error code.
    #[error("opus error {0}")]
    Code(i32),
}

type EncoderCreate = unsafe extern "C" fn(i32, c_int, c_int, *mut c_int) -> *mut c_void;
type Encode = unsafe extern "C" fn(*mut c_void, *const i16, c_int, *mut u8, i32) -> i32;
type DecoderCreate = unsafe extern "C" fn(i32, c_int, *mut c_int) -> *mut c_void;
type Decode = unsafe extern "C" fn(*mut c_void, *const u8, i32, *mut i16, c_int, c_int) -> c_int;
type Destroy = unsafe extern "C" fn(*mut c_void);

/// The calls, from one library that stays open for the process's life.
pub struct Lib {
    encoder_create: EncoderCreate,
    encode: Encode,
    encoder_destroy: Destroy,
    decoder_create: DecoderCreate,
    decode: Decode,
    decoder_destroy: Destroy,
}

static LIB: OnceLock<Lib> = OnceLock::new();
static LOADING: Mutex<()> = Mutex::new(());

fn open() -> Result<Lib, OpusError> {
    let mut last = String::from("no libopus found");
    for path in LIBRARY_PATHS {
        let Ok(c) = CString::new(*path) else { continue };
        // SAFETY: dlopen with a valid C string; the handle is never closed.
        let handle = unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if handle.is_null() {
            last = format!("{path} did not open");
            continue;
        }
        let sym = |name: &str| -> Option<*mut c_void> {
            let c = CString::new(name).ok()?;
            // SAFETY: dlsym on a live handle with a valid C string.
            let p = unsafe { libc::dlsym(handle, c.as_ptr() as *const c_char) };
            (!p.is_null()).then_some(p)
        };
        let found = (|| {
            Some(Lib {
                // SAFETY: each symbol is libopus's function of exactly this signature (opus.h).
                encoder_create: unsafe { std::mem::transmute::<*mut c_void, EncoderCreate>(sym("opus_encoder_create")?) },
                encode: unsafe { std::mem::transmute::<*mut c_void, Encode>(sym("opus_encode")?) },
                encoder_destroy: unsafe { std::mem::transmute::<*mut c_void, Destroy>(sym("opus_encoder_destroy")?) },
                decoder_create: unsafe { std::mem::transmute::<*mut c_void, DecoderCreate>(sym("opus_decoder_create")?) },
                decode: unsafe { std::mem::transmute::<*mut c_void, Decode>(sym("opus_decode")?) },
                decoder_destroy: unsafe { std::mem::transmute::<*mut c_void, Destroy>(sym("opus_decoder_destroy")?) },
            })
        })();
        match found {
            Some(lib) => return Ok(lib),
            None => last = format!("{path} has no Opus calls"),
        }
    }
    Err(OpusError::Load(last))
}

/// libopus, opened once it loads; a failure is tried again on the next call (a library installed later is found).
pub fn lib() -> Result<&'static Lib, OpusError> {
    if let Some(lib) = LIB.get() {
        return Ok(lib);
    }
    let _one_at_a_time = LOADING.lock();
    if let Some(lib) = LIB.get() {
        return Ok(lib);
    }
    let lib = open()?;
    Ok(LIB.get_or_init(|| lib))
}

/// An Opus encoder (`OpusEncoder *`).
pub struct Encoder {
    lib: &'static Lib,
    st: *mut c_void,
    channels: usize,
}

// SAFETY: an encoder is used by one owner at a time (`&mut self`), and libopus keeps no thread-local state.
unsafe impl Send for Encoder {}

impl Encoder {
    /// `rate` is one of 8000, 12000, 16000, 24000 or 48000; `channels` 1 or 2.
    pub fn new(rate: u32, channels: usize, application: c_int) -> Result<Self, OpusError> {
        let lib = lib()?;
        let mut err: c_int = 0;
        // SAFETY: a plain constructor call; the result is checked.
        let st = unsafe { (lib.encoder_create)(rate as i32, channels as c_int, application, &mut err) };
        if st.is_null() || err != 0 {
            return Err(OpusError::Code(err));
        }
        Ok(Self { lib, st, channels })
    }

    /// One frame of interleaved samples (2.5, 5, 10, 20, 40 or 60 ms of them) → one Opus packet.
    pub fn encode(&mut self, pcm: &[i16]) -> Result<Vec<u8>, OpusError> {
        let frame = pcm.len() / self.channels.max(1);
        let mut out = vec![0u8; MAX_PACKET_BYTES];
        // SAFETY: `pcm` holds `frame * channels` samples and `out` has the room it is said to have.
        let n = unsafe { (self.lib.encode)(self.st, pcm.as_ptr(), frame as c_int, out.as_mut_ptr(), out.len() as i32) };
        if n < 0 {
            return Err(OpusError::Code(n));
        }
        out.truncate(n as usize);
        Ok(out)
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: created by opus_encoder_create, destroyed once.
        unsafe { (self.lib.encoder_destroy)(self.st) }
    }
}

/// An Opus decoder (`OpusDecoder *`).
pub struct Decoder {
    lib: &'static Lib,
    st: *mut c_void,
    channels: usize,
}

// SAFETY: as Encoder.
unsafe impl Send for Decoder {}

impl Decoder {
    pub fn new(rate: u32, channels: usize) -> Result<Self, OpusError> {
        let lib = lib()?;
        let mut err: c_int = 0;
        // SAFETY: a plain constructor call; the result is checked.
        let st = unsafe { (lib.decoder_create)(rate as i32, channels as c_int, &mut err) };
        if st.is_null() || err != 0 {
            return Err(OpusError::Code(err));
        }
        Ok(Self { lib, st, channels })
    }

    /// One Opus packet → its interleaved samples. An empty packet is a lost one (libopus conceals it).
    pub fn decode(&mut self, packet: &[u8]) -> Result<Vec<i16>, OpusError> {
        let mut out = vec![0i16; MAX_FRAME_SAMPLES * 2 * self.channels.max(1)];
        let frame = out.len() / self.channels.max(1);
        let (data, len) = if packet.is_empty() { (std::ptr::null(), 0) } else { (packet.as_ptr(), packet.len() as i32) };
        // SAFETY: `out` has room for `frame` samples per channel; `data` is `len` bytes or null for a lost packet.
        let n = unsafe { (self.lib.decode)(self.st, data, len, out.as_mut_ptr(), frame as c_int, 0) };
        if n < 0 {
            return Err(OpusError::Code(n));
        }
        out.truncate(n as usize * self.channels);
        Ok(out)
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: created by opus_decoder_create, destroyed once.
        unsafe { (self.lib.decoder_destroy)(self.st) }
    }
}
