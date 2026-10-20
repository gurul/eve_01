//! The radio side of `ble.py`: what `BleakScanner` and `BleakClient` do for
//! it, on btleplug.
//!
//! macOS gives a CoreBluetooth-assigned UUID instead of a MAC address, so a
//! device's address there is that UUID, upper-case as bleak prints it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{anyhow, bail};
use async_trait::async_trait;
use btleplug::api::{Central, CentralEvent, CentralState, Characteristic, Manager as _, Peripheral as _, ScanFilter, WriteType};
use btleplug::platform::{Adapter, Manager, Peripheral};
use futures::stream::{BoxStream, StreamExt};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::{BleClient, BleDevice, BleTransport};

/// `BleakClient(device)`'s default connect timeout.
pub const CONNECT_TIMEOUT_SECS: f64 = 10.0;

/// The default adapter, scanned and connected through btleplug.
#[derive(Default)]
pub struct BtleplugTransport {
    /// The manager and its first adapter, made on first use and kept: a scan
    /// and the connect that follows must use the same adapter.
    radio: tokio::sync::OnceCell<(Manager, Adapter)>,
}

impl BtleplugTransport {
    pub fn new() -> Self {
        Self::default()
    }

    async fn adapter(&self) -> anyhow::Result<Adapter> {
        let (_, adapter) = self
            .radio
            .get_or_try_init(|| async {
                let manager = Manager::new().await?;
                let adapter = manager
                    .adapters()
                    .await?
                    .into_iter()
                    .next()
                    .ok_or_else(|| anyhow!("No Bluetooth adapters found."))?;
                Ok::<_, anyhow::Error>((manager, adapter))
            })
            .await?;
        Ok(adapter.clone())
    }
}

/// The address bleak reports for a peripheral.
fn peripheral_address(p: &Peripheral) -> String {
    #[cfg(target_os = "macos")]
    {
        p.id().to_string().to_uppercase()
    }
    #[cfg(not(target_os = "macos"))]
    {
        p.address().to_string()
    }
}

/// The id an advertisement-carrying scan event is about.
fn advert_id(event: CentralEvent) -> Option<btleplug::platform::PeripheralId> {
    match event {
        CentralEvent::DeviceDiscovered(id)
        | CentralEvent::DeviceUpdated(id)
        | CentralEvent::ManufacturerDataAdvertisement { id, .. }
        | CentralEvent::ServiceDataAdvertisement { id, .. }
        | CentralEvent::ServicesAdvertisement { id, .. }
        | CentralEvent::RssiUpdate { id, .. } => Some(id),
        _ => None,
    }
}

#[async_trait]
impl BleTransport for BtleplugTransport {
    type Peripheral = Peripheral;
    type Client = BtleplugClient;

    /// `BleakScanner.find_device_by_address(address, timeout)` when an address
    /// is given (compared case-insensitively, as bleak does), else
    /// `find_device_by_filter` on `(adv.local_name or d.name) or ""` starting
    /// with `name_prefix`. Only devices heard during this scan count.
    async fn find_device(
        &self,
        address: Option<&str>,
        name_prefix: &str,
        timeout_secs: f64,
    ) -> anyhow::Result<Option<BleDevice<Peripheral>>> {
        let adapter = self.adapter().await?;
        if adapter.adapter_state().await? == CentralState::PoweredOff {
            bail!("Bluetooth device is turned off");
        }
        let mut events = adapter.events().await?;
        adapter.start_scan(ScanFilter::default()).await?;
        let wanted = address.map(str::to_lowercase);
        let scan = async {
            while let Some(event) = events.next().await {
                let Some(id) = advert_id(event) else { continue };
                let Ok(p) = adapter.peripheral(&id).await else { continue };
                let props = p.properties().await.ok().flatten();
                let name = props.and_then(|props| props.advertisement_name.or(props.local_name));
                let addr = peripheral_address(&p);
                let hit = match &wanted {
                    Some(w) => addr.to_lowercase() == *w,
                    None => name.as_deref().unwrap_or("").starts_with(name_prefix),
                };
                if hit {
                    return Some(BleDevice { name, address: addr, peripheral: p });
                }
            }
            None
        };
        let timeout = Duration::try_from_secs_f64(timeout_secs).unwrap_or_default();
        let found = tokio::time::timeout(timeout, scan).await.unwrap_or(None);
        adapter.stop_scan().await?;
        Ok(found)
    }

    /// `BleakClient(device).__aenter__`: connect within the default timeout,
    /// then discover services (bleak does both before handing the client over).
    async fn connect(&self, device: &BleDevice<Peripheral>) -> anyhow::Result<BtleplugClient> {
        let adapter = self.adapter().await?;
        let events = adapter.events().await?;
        let p = device.peripheral.clone();
        if let Err(e) = p.connect_with_timeout(Duration::from_secs_f64(CONNECT_TIMEOUT_SECS)).await {
            // As bleak does on a timeout: cancel the pending connection, which
            // CoreBluetooth would otherwise keep trying forever.
            let _ = p.disconnect().await;
            return Err(e.into());
        }
        if let Err(e) = p.discover_services().await {
            let _ = p.disconnect().await;
            return Err(e.into());
        }
        let connected = Arc::new(AtomicBool::new(true));
        let watcher = tokio::spawn(watch_link(p.clone(), events, connected.clone()));
        Ok(BtleplugClient { peripheral: p, connected, watcher })
    }
}

/// Keep `BtleplugClient::is_connected` current: the adapter's disconnect
/// event clears it at once, and a once-a-second check of the peripheral
/// covers a platform that misses the event.
async fn watch_link(p: Peripheral, mut events: BoxStream<'static, CentralEvent>, connected: Arc<AtomicBool>) {
    let id = p.id();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let mut events_open = true;
    loop {
        tokio::select! {
            event = events.next(), if events_open => match event {
                Some(CentralEvent::DeviceDisconnected(gone)) if gone == id => break,
                Some(_) => {}
                None => events_open = false,
            },
            _ = tick.tick() => {
                if !p.is_connected().await.unwrap_or(false) {
                    break;
                }
            }
        }
    }
    connected.store(false, Ordering::SeqCst);
}

/// A connected peripheral: what `BleakClient` is inside `async with`.
pub struct BtleplugClient {
    peripheral: Peripheral,
    connected: Arc<AtomicBool>,
    watcher: JoinHandle<()>,
}

impl Drop for BtleplugClient {
    fn drop(&mut self) {
        self.watcher.abort();
    }
}

impl BtleplugClient {
    /// bleak's lookup of a characteristic by UUID string.
    fn characteristic(&self, uuid: &str) -> anyhow::Result<Characteristic> {
        let wanted = Uuid::parse_str(uuid)?;
        let mut found = self.peripheral.characteristics().into_iter().filter(|c| c.uuid == wanted);
        let first = found.next().ok_or_else(|| anyhow!("Characteristic {uuid} was not found!"))?;
        if found.next().is_some() {
            bail!(
                "Multiple Characteristics with this UUID, refer to your desired characteristic by the `handle` attribute instead."
            );
        }
        Ok(first)
    }
}

#[async_trait]
impl BleClient for BtleplugClient {
    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    fn mtu_size(&self) -> usize {
        usize::from(self.peripheral.mtu())
    }

    async fn start_notify(&self, uuid: &str) -> anyhow::Result<BoxStream<'static, Vec<u8>>> {
        let c = self.characteristic(uuid)?;
        let stream = self.peripheral.notifications().await?;
        self.peripheral.subscribe(&c).await?;
        let wanted = c.uuid;
        Ok(stream
            .filter_map(move |n| futures::future::ready((n.uuid == wanted).then_some(n.value)))
            .boxed())
    }

    async fn write_gatt_char(&self, uuid: &str, data: &[u8], response: bool) -> anyhow::Result<()> {
        let c = self.characteristic(uuid)?;
        let kind = if response { WriteType::WithResponse } else { WriteType::WithoutResponse };
        self.peripheral.write(&c, data, kind).await?;
        Ok(())
    }

    async fn disconnect(&self) -> anyhow::Result<()> {
        let result = self.peripheral.disconnect().await;
        self.connected.store(false, Ordering::SeqCst);
        result?;
        Ok(())
    }
}
