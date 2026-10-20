//! Port of `desk_call.py`: the board's hold-to-talk button runs the phone's push-to-talk call on the desk, with the
//! Mac microphone for the phone's audio and the Mac's or the Voice PE's speaker for eve's voice. See [`desk_call`].

pub mod audio_out;
pub mod desk_call;

pub use desk_call::*;

#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_lean;
