//! Port of `phone_call.py`: calling eve from the phone (the Mini App's **Call eve**, the Eve Link app, eve lite's
//! WebRTC call), push to talk into the same chat the owner texts, eve's replies spoken back, eve's expressions in
//! the phone's eyes, and notes taken on the phone's microphone. See [`phone_call`] for the whole story.

pub mod phone_call;
mod py;

pub use phone_call::*;
