//! The port of `eve_bridge/lights.py`: the owner's lights (Govee, WiZ, HappyLighting, Tuya) behind one
//! controller, the model's tools, the code path for a message that is only a light command, and the
//! `eve-bridge lights` CLI. See [`lights`] for the whole story.

pub mod cli;
pub mod govee;
pub mod lights;
pub mod py;
pub mod triones;
pub mod tuya;
pub mod wiz;

pub use cli::{cli, merge, scan};
pub use govee::{govee_commands, govee_scan, GoveeDriver, GoveeFound, GoveePorts, GOVEE_CMD_PORT, GOVEE_GROUP,
                GOVEE_KELVIN, GOVEE_LISTEN_PORT, GOVEE_SCAN_PORT};
pub use lights::*;
pub use triones::{triones_color, triones_frames, triones_parse_status, triones_scan, TrionesDriver, TrionesFound,
                  TRIONES_CONNECT_SECS, TRIONES_IDLE_SECS, TRIONES_NAMES, TRIONES_NOTIFY, TRIONES_OFF, TRIONES_ON,
                  TRIONES_STATUS, TRIONES_WRITE};
pub use tuya::{tuya_calls, tuya_scan, tuya_temp_percent, TuyaDriver, TuyaFound, TUYA_KELVIN};
pub use wiz::{wiz_call, wiz_call_signed, wiz_import, wiz_message, wiz_params, wiz_parse_pilot, wiz_scan, wiz_sign,
              WizDriver, WizFound, WIZ_KELVIN, WIZ_MIN_DIMMING, WIZ_PORT};

#[cfg(test)]
mod tests;
