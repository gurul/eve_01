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

/// `ble.send` returns Python's bool; the `LookupError` an unknown codec raises is logged here, a failed send.
fn sent(result: Result<bool, eve_protocol::LookupError>) -> bool {
    match result {
        Ok(ok) => ok,
        Err(e) => {
            tracing::error!("send: unknown wire codec ({})", e.0);
            false
        }
    }
}

#[async_trait::async_trait]
impl BoardLink for BleLink {
    fn connected(&self) -> bool {
        self.0.connected()
    }
    async fn wait_connected(&self) {
        self.0.wait_connected().await
    }
    async fn send(&self, obj: &Value, codec: Option<&str>) -> bool {
        sent(self.0.send(obj, codec).await)
    }
    async fn run(&self) {
        self.0.run().await
    }
    async fn stop(&self) {
        self.0.stop().await
    }
}

#[async_trait::async_trait]
impl BoardLink for SerialLink {
    fn connected(&self) -> bool {
        self.serial.connected()
    }
    async fn wait_connected(&self) {
        self.serial.wait_connected().await
    }
    async fn send(&self, obj: &Value, codec: Option<&str>) -> bool {
        // Every send to the robot, tee'd: the controller keeps the
        // heartbeat, time, conversation state and sound (controller.mirrored).
        let ok = match self.serial.send(obj, codec).await {
            Ok(ok) => ok,
            // Python's LookupError escaped before the mirror.
            Err(e) => return sent(Err(e)),
        };
        if let Some(ctl) = &self.controller {
            ctl.mirror(obj).await;
        }
        ok
    }
    async fn run(&self) {
        self.serial.run().await
    }
    async fn stop(&self) {
        self.serial.stop().await
    }
    fn wired(&self) -> bool {
        self.serial.wired()
    }
    fn pulse_reset(&self, why: &str) -> bool {
        self.serial.pulse_reset(why);
        true
    }
    fn force_reconnect(&self, why: &str) -> bool {
        self.serial.force_reconnect(why);
        true
    }
}

/// One line to `inbox`'s daemon, for the serial transport and the controller (their handlers return nothing, so
/// a failure is logged here, where Python's task would have reported it).
fn serial_handler(inbox: Inbox) -> eve_serial_transport::IncomingHandler {
    Arc::new(move |obj: serde_json::Map<String, Value>| {
        let inbox = inbox.clone();
        Box::pin(async move {
            let Some(daemon) = inbox.get().and_then(Weak::upgrade) else {
                return;
            };
            if let Err(e) = daemon.handle_ble(obj).await {
                tracing::error!("on_message handler crashed: {e:#}");
            }
        })
    })
}

/// The link `__init__` builds for these options, delivering incoming lines to `inbox`'s daemon
/// (`Daemon::handle_ble`), and the Voice PE controller when the serial transport has one (its input goes to
/// `Daemon::handle_ble` too, as Python's `on_input`).
pub fn make_link(
    opts: &crate::daemon::DaemonOptions,
    environ: Option<&crate::seams::Environ>,
    inbox: Inbox,
) -> (Arc<dyn BoardLink>, Option<Arc<eve_controller::Controller>>) {
    if let Some(port) = opts.serial_port.as_deref().filter(|p| !p.is_empty()) {
        // USB CDC transport (Freenove FNK0104B port): same wire protocol,
        // different pipe. Kept on the .ble attribute so the rest of the
        // daemon doesn't care which transport is live.
        //
        // A controller board (controller.py, the Voice PE) is named by its
        // USB serial; the robot's glob must never open it, nor any board in
        // EVE_SERIAL_SKIP.
        let ctl_serial = eve_controller::controller_serial(environ);
        let mut skip = eve_serial_transport::serial_skip(environ);
        if let Some(ctl) = &ctl_serial {
            skip.insert(ctl.to_lowercase());
        }
        let serial = Arc::new(eve_serial_transport::EveSerial::new(serial_handler(inbox.clone()), port, 115200, skip));
        // A board reboot under an unbroken CH340 link never fires the
        // on-connect resync — replay it when the boot banner scrolls past,
        // or the reborn board keeps "--:--" and "No Claude" indefinitely.
        let boot_inbox = inbox.clone();
        serial.set_on_boot(Some(Arc::new(move || {
            let inbox = boot_inbox.clone();
            Box::pin(async move {
                if let Some(daemon) = inbox.get().and_then(Weak::upgrade) {
                    daemon.resync_board().await;
                }
            })
        })));
        let controller = ctl_serial
            .map(|ctl| Arc::new(eve_controller::Controller::new(&ctl, serial_handler(inbox.clone()))));
        let link: Arc<dyn BoardLink> = Arc::new(SerialLink { serial, controller: controller.clone() });
        return (link, controller);
    }
    let handler: eve_ble::IncomingHandler = Arc::new(move |obj: Value| {
        let inbox = inbox.clone();
        Box::pin(async move {
            let Some(daemon) = inbox.get().and_then(Weak::upgrade) else {
                return Ok(());
            };
            match obj {
                Value::Object(obj) => daemon.handle_ble(obj).await,
                other => {
                    tracing::debug!("ble: unhandled {other}");
                    Ok(())
                }
            }
        })
    });
    let ble = eve_ble::EveBle::new(handler, Some(&opts.device_name_prefix), opts.device_address.as_deref());
    (Arc::new(BleLink(Arc::new(ble))), None)
}
