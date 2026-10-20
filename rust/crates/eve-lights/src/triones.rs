//! HappyLighting: the "Triones" protocol QH-tek's controllers speak (`QHM-…` names) over Bluetooth LE: write
//! `ffd9`, status on `ffd4`.

use std::sync::{Arc, Weak};
use std::time::Duration;

use async_trait::async_trait;
use btleplug::api::{Central, Characteristic, Manager as _, Peripheral as _, ScanFilter, WriteType};
use btleplug::platform::{Adapter, Manager, Peripheral};
use futures::StreamExt;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::lights::{rgb_percent, scale_rgb, Change, Driver, Light, LightError, Rgb, State};

pub const TRIONES_WRITE: &str = "0000ffd9-0000-1000-8000-00805f9b34fb";
pub const TRIONES_NOTIFY: &str = "0000ffd4-0000-1000-8000-00805f9b34fb";
pub const TRIONES_ON: [u8; 3] = [0xCC, 0x23, 0x33];
pub const TRIONES_OFF: [u8; 3] = [0xCC, 0x24, 0x33];
pub const TRIONES_STATUS: [u8; 3] = [0xEF, 0x01, 0x77];
pub const TRIONES_NAMES: [&str; 4] = ["QHM-", "Triones", "LEDBlue", "LEDBLE"];
/// hold the connection this long for the next command; the phone app needs it back
pub const TRIONES_IDLE_SECS: f64 = 20.0;
pub const TRIONES_CONNECT_SECS: f64 = 10.0;

pub fn triones_color(rgb: &[i64]) -> Vec<u8> {
    let ch = |i: usize| rgb.get(i).copied().unwrap_or(0).clamp(0, 255) as u8;
    vec![0x56, ch(0), ch(1), ch(2), 0x00, 0xF0, 0xAA]
}

/// `66 tt PP mm .. ss RR GG BB WW vv 99`: PP 0x23 on, 0x24 off; mode 0x41 is a steady colour.
pub fn triones_parse_status(data: &[u8]) -> Option<State> {
    if data.len() < 12 || data[0] != 0x66 || data[data.len() - 1] != 0x99 {
        return None;
    }
    let rgb: Rgb = [data[6] as i64, data[7] as i64, data[8] as i64];
    Some(State { on: Some(data[2] == 0x23), rgb: Some(rgb), brightness: Some(rgb_percent(&rgb)), kelvin: None })
}

/// The writes for a change. The controller has no brightness of its own: brightness is the colour's level, so
/// a colour keeps the current level and a level keeps the current colour.
pub fn triones_frames(change: &Change, current: Option<&State>) -> Vec<Vec<u8>> {
    let c = change.normalised();
    if c.power == Some(false) {
        return vec![TRIONES_OFF.to_vec()];
    }
    let mut out = Vec::new();
    let was_on = current.map(|s| s.on == Some(true)).unwrap_or(false);
    if c.power == Some(true) || !c.is_empty() {
        out.push(TRIONES_ON.to_vec());
    }
    if c.color.is_none() && c.brightness.is_none() {
        return out;
    }
    let base: Rgb = match (&c.color, current.and_then(|s| s.rgb)) {
        (Some(color), _) => color.as_rgb(),
        (None, Some(rgb)) => rgb,
        (None, None) => [255, 255, 255],
    };
    let level = match (c.brightness, current.and_then(|s| s.rgb)) {
        (Some(b), _) => b,
        (None, Some(rgb)) if was_on && rgb_percent(&rgb) > 0 => rgb_percent(&rgb),
        _ => 100,
    };
    out.push(triones_color(&scale_rgb(&base, level)));
    out
}

fn ble_error(e: btleplug::Error) -> LightError {
    LightError::py("BleakError", e.to_string())
}

/// The address `lights.json` keeps for a controller: on macOS the CoreBluetooth identifier (upper-case, as
/// CoreBluetooth prints it), elsewhere the MAC.
async fn address_of(p: &Peripheral) -> String {
    if cfg!(target_os = "macos") {
        p.id().to_string().to_uppercase()
    } else {
        p.address().to_string()
    }
}

async fn first_adapter() -> Result<Adapter, LightError> {
    let manager = Manager::new().await.map_err(ble_error)?;
    manager
        .adapters()
        .await
        .map_err(ble_error)?
        .into_iter()
        .next()
        .ok_or_else(|| LightError::py("BleakError", "Bluetooth device is turned off"))
}

/// Scan until the controller with this address shows up, or the time runs out.
async fn find_device_by_address(adapter: &Adapter, address: &str, timeout: Duration) -> Result<Option<Peripheral>, LightError> {
    adapter.start_scan(ScanFilter::default()).await.map_err(ble_error)?;
    let deadline = tokio::time::Instant::now() + timeout;
    let found = loop {
        let mut hit = None;
        for p in adapter.peripherals().await.map_err(ble_error)? {
            if address_of(&p).await.eq_ignore_ascii_case(address) {
                hit = Some(p);
                break;
            }
        }
        if hit.is_some() || tokio::time::Instant::now() >= deadline {
            break hit;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let _ = adapter.stop_scan().await;
    Ok(found)
}

struct Conn {
    peripheral: Peripheral,
    write: Characteristic,
    listener: JoinHandle<()>,
}

struct Inner {
    light: Light,
    lock: tokio::sync::Mutex<()>,
    adapter: tokio::sync::Mutex<Option<Adapter>>,
    client: parking_lot::Mutex<Option<Conn>>,
    closer: parking_lot::Mutex<Option<JoinHandle<()>>>,
    status: Arc<parking_lot::Mutex<Option<oneshot::Sender<Vec<u8>>>>>,
}

/// One Bluetooth connection, opened on demand and closed after TRIONES_IDLE_SECS of quiet.
pub struct TrionesDriver {
    inner: Arc<Inner>,
}

impl TrionesDriver {
    pub fn new(light: Light) -> Self {
        TrionesDriver {
            inner: Arc::new(Inner {
                light,
                lock: tokio::sync::Mutex::new(()),
                adapter: tokio::sync::Mutex::new(None),
                client: parking_lot::Mutex::new(None),
                closer: parking_lot::Mutex::new(None),
                status: Arc::new(parking_lot::Mutex::new(None)),
            }),
        }
    }
}

impl Inner {
    async fn adapter(&self) -> Result<Adapter, LightError> {
        let mut slot = self.adapter.lock().await;
        if let Some(a) = slot.as_ref() {
            return Ok(a.clone());
        }
        let a = first_adapter().await?;
        *slot = Some(a.clone());
        Ok(a)
    }

    async fn connect(&self) -> Result<(Peripheral, Characteristic), LightError> {
        let held = self.client.lock().as_ref().map(|c| (c.peripheral.clone(), c.write.clone()));
        if let Some((p, w)) = held {
            if p.is_connected().await.unwrap_or(false) {
                return Ok((p, w));
            }
        }
        let adapter = self.adapter().await?;
        let wait = Duration::from_secs_f64(TRIONES_CONNECT_SECS);
        let dev = find_device_by_address(&adapter, &self.light.address, wait)
            .await?
            .ok_or_else(|| LightError::Connection("the Bluetooth light is out of range or taken by the phone app".into()))?;
        match tokio::time::timeout(wait, dev.connect()).await {
            Ok(r) => r.map_err(ble_error)?,
            Err(_) => return Err(LightError::Timeout(String::new())),
        }
        dev.discover_services().await.map_err(ble_error)?;
        let chars = dev.characteristics();
        let find = |uuid: &str| {
            let want = Uuid::parse_str(uuid).ok();
            chars
                .iter()
                .find(|c| Some(c.uuid) == want)
                .cloned()
                .ok_or_else(|| LightError::py("BleakError", format!("Characteristic {uuid} was not found!")))
        };
        let write = find(TRIONES_WRITE)?;
        let notify = find(TRIONES_NOTIFY)?;
        dev.subscribe(&notify).await.map_err(ble_error)?;
        let mut stream = dev.notifications().await.map_err(ble_error)?;
        let status = self.status.clone();
        let notify_uuid = notify.uuid;
        let listener = tokio::spawn(async move {
            while let Some(n) = stream.next().await {
                if n.uuid == notify_uuid && n.value.first() == Some(&0x66) {
                    if let Some(tx) = status.lock().take() {
                        let _ = tx.send(n.value);
                    }
                }
            }
        });
        let old = self.client.lock().replace(Conn { peripheral: dev.clone(), write: write.clone(), listener });
        if let Some(old) = old {
            old.listener.abort();
        }
        Ok((dev, write))
    }

    async fn read(&self, p: &Peripheral, write: &Characteristic) -> Result<Option<State>, LightError> {
        let (tx, rx) = oneshot::channel();
        *self.status.lock() = Some(tx);
        let out = async {
            p.write(write, &TRIONES_STATUS, WriteType::WithoutResponse).await.map_err(ble_error)?;
            match tokio::time::timeout(Duration::from_secs(2), rx).await {
                Ok(Ok(data)) => Ok(triones_parse_status(&data)),
                _ => Ok(None),
            }
        }
        .await;
        *self.status.lock() = None;
        out
    }

    async fn close(&self, cancel_closer: bool) {
        let closer = self.closer.lock().take();
        if let (true, Some(h)) = (cancel_closer, closer) {
            h.abort();
        }
        let client = self.client.lock().take();
        if let Some(c) = client {
            c.listener.abort();
            // a connection that is already gone is closed
            let _ = c.peripheral.disconnect().await;
        }
    }
}

impl TrionesDriver {
    fn touch(&self) {
        let weak: Weak<Inner> = Arc::downgrade(&self.inner);
        let task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs_f64(TRIONES_IDLE_SECS)).await;
            if let Some(inner) = weak.upgrade() {
                inner.close(false).await;
            }
        });
        if let Some(old) = self.inner.closer.lock().replace(task) {
            old.abort();
        }
    }
}

#[async_trait]
impl Driver for TrionesDriver {
    async fn state(&self) -> Result<State, LightError> {
        let _held = self.inner.lock.lock().await;
        let (p, w) = self.inner.connect().await?;
        self.touch();
        self.inner
            .read(&p, &w)
            .await?
            .ok_or_else(|| LightError::Timeout("the Bluetooth light did not report its state".into()))
    }

    async fn apply(&self, change: &Change) -> Result<(), LightError> {
        let _held = self.inner.lock.lock().await;
        let (p, w) = self.inner.connect().await?;
        self.touch();
        let c = change.normalised();
        let needs_state = c.power != Some(false) && (c.color.is_some() || c.brightness.is_some());
        let current = if needs_state { self.inner.read(&p, &w).await? } else { None };
        for frame in triones_frames(change, current.as_ref()) {
            p.write(&w, &frame, WriteType::WithoutResponse).await.map_err(ble_error)?;
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Ok(())
    }

    async fn close(&self) -> Result<(), LightError> {
        self.inner.close(true).await;
        Ok(())
    }
}

/// A HappyLighting controller nearby: `{"address", "name"}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrionesFound {
    pub address: String,
    pub name: String,
}

/// Nearby HappyLighting controllers. Needs Bluetooth permission (see the crate doc).
pub async fn triones_scan(seconds: f64) -> Result<Vec<TrionesFound>, LightError> {
    let adapter = first_adapter().await?;
    adapter.start_scan(ScanFilter::default()).await.map_err(ble_error)?;
    tokio::time::sleep(Duration::from_secs_f64(seconds)).await;
    let peripherals = adapter.peripherals().await.map_err(ble_error);
    let _ = adapter.stop_scan().await;
    let mut out = Vec::new();
    for p in peripherals? {
        let props = p.properties().await.map_err(ble_error)?;
        let name = props.and_then(|pr| pr.local_name.or(pr.advertisement_name)).unwrap_or_default();
        if TRIONES_NAMES.iter().any(|prefix| name.starts_with(prefix)) {
            out.push(TrionesFound { address: address_of(&p).await, name });
        }
    }
    Ok(out)
}
