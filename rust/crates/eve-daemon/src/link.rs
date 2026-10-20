//! The board link: `self.ble` as `__init__` picks it. BLE by default; with `serial_port`, the USB CDC transport,
//! tee'd to the Voice PE (`self._controller.mirror`) when EVE_CONTROLLER_SERIAL names one.

use std::sync::{Arc, OnceLock, Weak};

use serde_json::Value;

use crate::daemon::Daemon;
use crate::seams::BoardLink;

/// Where the link's incoming lines go: the daemon, bound once it exists (the link is made before the daemon,
/// Python's `EveBle(on_message=self._handle_ble)` closed over a half-built `self`). Lines before the bind are
/// dropped.
pub type Inbox = Arc<OnceLock<Weak<Daemon>>>;

/// [`BoardLink`] over `eve_ble::EveBle`.
pub struct BleLink(pub Arc<eve_ble::EveBle>);

/// [`BoardLink`] over `eve_serial_transport::EveSerial`, mirroring every line it sends to the Voice PE when one
/// is attached (Python wrapped `send` to tee it).
pub struct SerialLink {
    pub serial: Arc<eve_serial_transport::EveSerial>,
    pub controller: Option<Arc<eve_controller::Controller>>,
}

#[async_trait::async_trait]
impl BoardLink for BleLink {
    fn connected(&self) -> bool {
        todo!()
    }
    async fn wait_connected(&self) {
        todo!()
    }
    async fn send(&self, obj: &Value, codec: Option<&str>) -> bool {
        todo!()
    }
    async fn run(&self) {
        todo!()
    }
    async fn stop(&self) {
        todo!()
    }
}

#[async_trait::async_trait]
impl BoardLink for SerialLink {
    fn connected(&self) -> bool {
        todo!()
    }
    async fn wait_connected(&self) {
        todo!()
    }
    async fn send(&self, obj: &Value, codec: Option<&str>) -> bool {
        todo!()
    }
    async fn run(&self) {
        todo!()
    }
    async fn stop(&self) {
        todo!()
    }
    fn wired(&self) -> bool {
        todo!()
    }
    fn pulse_reset(&self, why: &str) -> bool {
        todo!()
    }
    fn force_reconnect(&self, why: &str) -> bool {
        todo!()
    }
}

/// The link `__init__` builds for these options, delivering incoming lines to `inbox`'s daemon
/// (`Daemon::handle_ble`), and the Voice PE controller when the serial transport has one (its input goes to
/// `Daemon::handle_ble` too, as Python's `on_input`).
pub fn make_link(
    opts: &crate::daemon::DaemonOptions,
    environ: Option<&crate::seams::Environ>,
    inbox: Inbox,
) -> (Arc<dyn BoardLink>, Option<Arc<eve_controller::Controller>>) {
    todo!()
}
