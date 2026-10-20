//! Pika, the owner's ELEGOO robot car, driven from the Eve Link app through this Mac: the hold-to-drive panel and
//! the car's video, over the link tunnel ([`pika`]) or straight to the phone over WebRTC ([`pika_rtc`]).
//! Ports `eve_bridge/pika.py` and `eve_bridge/pika_rtc.py`.

pub mod pika;
pub mod pika_rtc;

pub use pika::{make_pika, Pika, PikaConfig, PikaError, PikaOptions};
pub use pika_rtc::PikaRtc;
