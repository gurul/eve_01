//! BLE client that pairs with a claude-desktop-buddy device over Nordic UART Service.
//!
//! Scans for peripherals whose advertised local name starts with "Claude", connects,
//! subscribes to TX notifications, and exposes an async `send()` method that writes
//! newline-terminated JSON to RX.
//!
//! Port of `eve_bridge/ble.py`. The radio goes through btleplug (`btle.rs`),
//! behind the [`BleTransport`] / [`BleClient`] seam; sleeps and the monotonic
//! clock go through [`Clock`], so the reconnect state machine can be driven
//! deterministically in tests.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use eve_protocol::{Codec, LineAssembler, LookupError, NUS_RX_UUID, NUS_TX_UUID, encode};
use futures::FutureExt;
use futures::future::BoxFuture;
use futures::stream::{BoxStream, StreamExt};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

mod btle;
mod decoder;

pub use btle::{BtleplugClient, BtleplugTransport, CONNECT_TIMEOUT_SECS};

use decoder::IncrementalDecoder;

// Default scan parameters.
pub const DEFAULT_NAME_PREFIX: &str = "Claude";
/// Windows uses a short 3s scan: the radio-reset recovery (below) keys its
/// threshold timing off fast scan cycles, and BLE adverts arrive every
/// 100-500ms so 3s is ample to catch a present device. Other platforms keep the
/// original 10s for more discovery margin in congested 2.4GHz environments or
/// with long advertising intervals — the short timeout was a Windows-specific
/// need and shouldn't narrow macOS/Linux discovery.
pub const SCAN_TIMEOUT_SECS: f64 = if cfg!(target_os = "windows") { 3.0 } else { 10.0 };

/// Exponential backoff for reconnection: if the device is resetting or rejecting
/// us, we don't want to hammer it. After each failure we double the wait up to
/// RECONNECT_BACKOFF_MAX; a successful connection that survives at least
/// STABLE_CONNECTION_SECS resets the backoff.
pub const RECONNECT_BACKOFF_BASE_SECS: f64 = 3.0;
pub const RECONNECT_BACKOFF_MAX_SECS: f64 = 60.0;
pub const STABLE_CONNECTION_SECS: f64 = 30.0;

/// Windows: BluetoothLEAdvertisementWatcher can silently stop delivering
/// callbacks while reporting status=Started. After this many consecutive
/// scan-timeout misses with the radio ON, we programmatically toggle the
/// radio off→on to recover without user intervention.
///
/// Kept deliberately conservative: power-cycling the radio briefly drops ALL
/// of the user's Bluetooth devices (mouse, keyboard, headphones), so we only
/// resort to it after a sustained run of misses (~25s at a 3s scan timeout +
/// base backoff) rather than reacting to a transient scan glitch.
pub const RADIO_RESET_AFTER_MISSES: u64 = 5;

/// A single power-cycle doesn't always revive the watcher. Re-arm the reset
/// after each fresh run of RADIO_RESET_AFTER_MISSES misses, but cap the total
/// attempts per disconnected spell and enforce a cooldown between them — so a
/// genuinely dead watcher is retried (instead of giving up after one shot and
/// stranding the user back at manual BT-toggling), without power-cycling the
/// user's whole radio on a tight loop. The counter and cap reset on a successful
/// connect. After the cap we stop trying and fall back to plain backoff.
pub const RADIO_RESET_MAX_ATTEMPTS: u32 = 3;
pub const RADIO_RESET_COOLDOWN_SECS: f64 = 120.0;

/// Handler for lines received from the stick (device → daemon).
pub type IncomingHandler = Arc<dyn Fn(Value) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;

/// `asyncio.sleep` and `time.monotonic`, as `ble.py` uses them.
pub trait Clock: Send + Sync + 'static {
    fn sleep(&self, secs: f64) -> BoxFuture<'static, ()>;
    /// Seconds on a clock that never goes back.
    fn monotonic(&self) -> f64;
}

/// The real clock: tokio sleep, `Instant` time.
pub struct TokioClock {
    origin: Instant,
}

impl Default for TokioClock {
    fn default() -> Self {
        TokioClock { origin: Instant::now() }
    }
}

impl Clock for TokioClock {
    fn sleep(&self, secs: f64) -> BoxFuture<'static, ()> {
        let d = Duration::try_from_secs_f64(secs.max(0.0)).unwrap_or(Duration::MAX);
        tokio::time::sleep(d).boxed()
    }

    fn monotonic(&self) -> f64 {
        self.origin.elapsed().as_secs_f64()
    }
}

/// A scan result: bleak's `BLEDevice`.
#[derive(Debug, Clone)]
pub struct BleDevice<P> {
    pub name: Option<String>,
    pub address: String,
    pub peripheral: P,
}

/// A connected client: the parts of `BleakClient` `ble.py` uses.
#[async_trait]
pub trait BleClient: Send + Sync + 'static {
    fn is_connected(&self) -> bool;
    /// ATT MTU, including the 3-byte header.
    fn mtu_size(&self) -> usize;
    /// Subscribe to a characteristic; the stream yields each notification's value.
    async fn start_notify(&self, uuid: &str) -> anyhow::Result<BoxStream<'static, Vec<u8>>>;
    async fn write_gatt_char(&self, uuid: &str, data: &[u8], response: bool) -> anyhow::Result<()>;
    async fn disconnect(&self) -> anyhow::Result<()>;
}

/// Finding, connecting to and recovering the radio: `BleakScanner`,
/// `BleakClient(device)` and the WinRT radio toggle.
#[async_trait]
pub trait BleTransport: Send + Sync + 'static {
    type Peripheral: Send + Sync + 'static;
    type Client: BleClient;

    /// The first device whose address matches (when one is given) or whose
    /// advertised name starts with `name_prefix`, within `timeout_secs`.
    async fn find_device(
        &self,
        address: Option<&str>,
        name_prefix: &str,
        timeout_secs: f64,
    ) -> anyhow::Result<Option<BleDevice<Self::Peripheral>>>;

    async fn connect(&self, device: &BleDevice<Self::Peripheral>) -> anyhow::Result<Self::Client>;

    /// See [`try_reset_radio`].
    async fn try_reset_radio(&self, clock: &dyn Clock) -> bool {
        try_reset_radio(clock).await
    }
}

/// How one pass of the reconnect loop ends inside its `try`.
enum Step {
    /// `continue`: straight to the next pass, no reconnect wait.
    Continue,
    /// Fall through to the reconnect wait.
    Wait,
}

pub struct EveBle<T: BleTransport = BtleplugTransport> {
    pub on_message: IncomingHandler,
    pub name_prefix: String,
    /// If provided, skip scanning.
    pub address: Option<String>,
    transport: T,
    clock: Arc<dyn Clock>,
    client: parking_lot::Mutex<Option<Arc<T::Client>>>,
    connected_evt: watch::Sender<bool>,
    send_lock: tokio::sync::Mutex<()>,
    stop: Arc<AtomicBool>,
}

impl EveBle<BtleplugTransport> {
    /// `EveBLE(on_message, name_prefix=DEFAULT_NAME_PREFIX, address=None)`.
    pub fn new(on_message: IncomingHandler, name_prefix: Option<&str>, address: Option<&str>) -> Self {
        EveBle::with_transport(
            on_message,
            name_prefix,
            address,
            BtleplugTransport::new(),
            Arc::new(TokioClock::default()),
        )
    }
}

impl<T: BleTransport> EveBle<T> {
    pub fn with_transport(
        on_message: IncomingHandler,
        name_prefix: Option<&str>,
        address: Option<&str>,
        transport: T,
        clock: Arc<dyn Clock>,
    ) -> Self {
        EveBle {
            on_message,
            name_prefix: name_prefix.unwrap_or(DEFAULT_NAME_PREFIX).to_owned(),
            address: address.map(str::to_owned),
            transport,
            clock,
            client: parking_lot::Mutex::new(None),
            connected_evt: watch::Sender::new(false),
            send_lock: tokio::sync::Mutex::new(()),
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    fn current_client(&self) -> Option<Arc<T::Client>> {
        self.client.lock().clone()
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    pub fn connected(&self) -> bool {
        self.current_client().is_some_and(|c| c.is_connected())
    }

    pub async fn wait_connected(&self) {
        let mut rx = self.connected_evt.subscribe();
        // The sender lives in `self`, so it cannot close while we wait.
        let _ = rx.wait_for(|set| *set).await;
    }

    /// Write a newline-terminated JSON object to the stick's RX. Returns true on success.
    ///
    /// `codec` is passed through to `protocol::encode`. Default (`None`)
    /// means UTF-8 JSON; `"gbk"` / `"big5"` / `"shift_jis"` switch the
    /// wire to the matching CJK firmware variant's expected byte encoding
    /// (see `protocol::CJK_CODECS`).
    ///
    /// `Err` is the `LookupError` `encode` raises for an unknown codec, which
    /// Python lets escape `send` too.
    pub async fn send(&self, obj: &Value, codec: Option<&str>) -> Result<bool, LookupError> {
        let Some(client) = self.current_client().filter(|c| c.is_connected()) else {
            return Ok(false);
        };
        let data = encode(obj, codec)?;
        let sent: anyhow::Result<()> = async {
            let _guard = self.send_lock.lock().await;
            // ATT Write Without Response payload = MTU - 3 bytes overhead.
            // Chunk so a multi-byte character (UTF-8, or the CJK codec's two bytes) never straddles a
            // packet boundary.
            let chunk_size = client.mtu_size().saturating_sub(3).max(20);
            for chunk in codec_safe_chunks(&data, chunk_size, codec)? {
                client.write_gatt_char(NUS_RX_UUID, chunk, false).await?;
                // Yield so the BLE host stack can drain Write Without Response
                // credits before the next chunk; prevents silent drops.
                tokio::task::yield_now().await;
            }
            Ok(())
        }
        .await;
        match sent {
            Ok(()) => Ok(true),
            Err(e) => {
                warn!("ble send failed: {e}");
                Ok(false)
            }
        }
    }

    /// Long-running connect/serve/reconnect loop. Exits when stop() is called.
    ///
    /// Uses exponential backoff so a misbehaving peripheral (e.g. firmware in
    /// a reset loop, bonding confusion) gets breathing room instead of being
    /// hammered every 3 seconds.
    pub async fn run(&self) {
        let mut backoff = RECONNECT_BACKOFF_BASE_SECS;
        let mut consecutive_misses: u64 = 0;
        // Radio-reset recovery state. Re-armed by the miss counter (not a
        // one-shot flag): another reset is allowed once misses reach a fresh
        // RADIO_RESET_AFTER_MISSES beyond the last reset, capped at
        // RADIO_RESET_MAX_ATTEMPTS and gated by RADIO_RESET_COOLDOWN_SECS, until
        // a successful connect clears it. This makes a dead watcher retryable
        // instead of giving up after one shot.
        let mut reset_attempts: u32 = 0;
        let mut misses_at_last_reset: u64 = 0;
        // Python's `last_reset_ts = 0.0` sentinel is `None` here.
        let mut last_reset_ts: Option<f64> = None;
        while !self.stopped() {
            let mut connect_ts: Option<f64> = None;
            let step: anyhow::Result<Step> = async {
                let Some(device) = self.find_device().await? else {
                    consecutive_misses += 1;
                    info!("no eve device found, retrying in {backoff:.1}s (miss #{consecutive_misses})");
                    // Eligible for a radio reset when: we've hit a fresh run of
                    // RADIO_RESET_AFTER_MISSES since the last attempt, we're under
                    // the attempt cap, and the cooldown has elapsed.
                    let now = self.clock.monotonic();
                    let eligible = consecutive_misses.saturating_sub(misses_at_last_reset) >= RADIO_RESET_AFTER_MISSES
                        && reset_attempts < RADIO_RESET_MAX_ATTEMPTS
                        && last_reset_ts.is_none_or(|ts| now - ts >= RADIO_RESET_COOLDOWN_SECS);
                    let mut reset_fired = false;
                    if eligible {
                        reset_fired = self.try_reset_radio().await;
                        if reset_fired {
                            reset_attempts += 1;
                            misses_at_last_reset = consecutive_misses;
                            last_reset_ts = Some(self.clock.monotonic());
                            if reset_attempts >= RADIO_RESET_MAX_ATTEMPTS {
                                warn!(
                                    "radio reset: {reset_attempts} attempts made without recovery — \
                                     giving up auto-reset; may need a manual BT toggle"
                                );
                            }
                        }
                    }
                    if reset_fired {
                        // Radio was power-cycled (already slept ~4s inside). Give the
                        // device a fresh fast attempt at the base backoff.
                        backoff = RECONNECT_BACKOFF_BASE_SECS;
                        return Ok(Step::Continue);
                    }
                    // No reset this iteration (below threshold, capped, cooling
                    // down, or a no-op platform/path). Back off with a real sleep
                    // so we never busy-loop.
                    self.clock.sleep(backoff).await;
                    backoff = (backoff * 2.0).min(RECONNECT_BACKOFF_MAX_SECS);
                    return Ok(Step::Continue);
                };
                consecutive_misses = 0;
                // Successful find: clear all reset state so the next disconnected
                // spell starts fresh and is fully retryable again.
                reset_attempts = 0;
                misses_at_last_reset = 0;
                last_reset_ts = None;
                info!(
                    "connecting to {} ({})",
                    device.name.as_deref().unwrap_or("None"),
                    device.address
                );
                // `async with BleakClient(device) as client:`
                let client = Arc::new(self.transport.connect(&device).await?);
                let mut reader: Option<JoinHandle<()>> = None;
                let body: anyhow::Result<()> = async {
                    *self.client.lock() = Some(client.clone());
                    let notes = client.start_notify(NUS_TX_UUID).await?;
                    reader = Some(self.spawn_reader(notes));
                    self.connected_evt.send_replace(true);
                    let ts = self.clock.monotonic();
                    connect_ts = Some(ts);
                    info!("connected, subscribed to TX notify");
                    // Hold the connection open until it drops or we're told to stop.
                    while client.is_connected() && !self.stopped() {
                        self.clock.sleep(1.0).await;
                    }
                    // Clear the connected event the instant we observe the drop —
                    // NOT in the cleanup below. Client teardown can take a while,
                    // and during that window `connected` is already false while the
                    // event is still set. A consumer that loops on wait_connected()
                    // (the daemon's on-connected task) would then wake on the stale
                    // event, find connected false, and busy-loop with no await until
                    // teardown finally clears it. Clear it here to close that race.
                    self.connected_evt.send_replace(false);
                    let lifetime = self.clock.monotonic() - ts;
                    info!("disconnected after {lifetime:.1}s");
                    // Clear reset state so the post-disconnect reconnect spell is
                    // fully retryable from scratch (fresh attempt budget + no
                    // stale cooldown).
                    reset_attempts = 0;
                    misses_at_last_reset = 0;
                    last_reset_ts = None;
                    Ok(())
                }
                .await;
                // `__aexit__`: disconnect; an error there replaces the body's.
                let exit = client.disconnect().await;
                if let Some(reader) = reader {
                    reader.abort();
                }
                exit?;
                body?;
                Ok(Step::Wait)
            }
            .await;
            if let Err(e) = &step {
                warn!("ble connection error: {e}");
            }
            // finally
            *self.client.lock() = None;
            self.connected_evt.send_replace(false);
            if matches!(step, Ok(Step::Continue)) {
                continue;
            }
            if !self.stopped() {
                // Reset backoff if the last connection was stable for a while —
                // a brief single disconnect shouldn't inherit flapping penalty.
                if connect_ts.is_some_and(|ts| self.clock.monotonic() - ts >= STABLE_CONNECTION_SECS) {
                    backoff = RECONNECT_BACKOFF_BASE_SECS;
                }
                info!("waiting {backoff:.1}s before reconnect");
                self.clock.sleep(backoff).await;
                backoff = (backoff * 2.0).min(RECONNECT_BACKOFF_MAX_SECS);
            }
        }
    }

    pub async fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(client) = self.current_client().filter(|c| c.is_connected()) {
            let _ = client.disconnect().await;
        }
    }

    // ---- internals ----

    /// Windows only: power-cycle the BT radio. See [`try_reset_radio`].
    async fn try_reset_radio(&self) -> bool {
        self.transport.try_reset_radio(self.clock.as_ref()).await
    }

    async fn find_device(&self) -> anyhow::Result<Option<BleDevice<T::Peripheral>>> {
        self.transport
            .find_device(self.address.as_deref(), &self.name_prefix, SCAN_TIMEOUT_SECS)
            .await
    }

    /// `start_notify(NUS_TX_UUID, self._on_notify)`: feed each notification
    /// to a fresh line assembler and hand every complete object to the
    /// handler on its own task.
    fn spawn_reader(&self, mut notes: BoxStream<'static, Vec<u8>>) -> JoinHandle<()> {
        let handler = self.on_message.clone();
        tokio::spawn(async move {
            let mut assembler = LineAssembler::new();
            while let Some(data) = notes.next().await {
                for obj in assembler.feed(&data) {
                    tokio::spawn(dispatch(handler.clone(), obj));
                }
            }
        })
    }

    #[cfg(test)]
    pub(crate) fn connected_event_is_set(&self) -> bool {
        *self.connected_evt.borrow()
    }
}

/// Run the handler for one incoming object; a failure is logged, never raised.
async fn dispatch(handler: IncomingHandler, obj: Value) {
    match AssertUnwindSafe(handler(obj)).catch_unwind().await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => error!("on_message handler crashed: {e:?}"),
        Err(_) => error!("on_message handler crashed: panicked"),
    }
}

/// Windows only: toggle the BT radio off→on to unstick the WinRT
/// advertisement watcher. The watcher can silently stop delivering
/// callbacks while reporting status=Started; a radio power-cycle clears
/// it without user intervention.
///
/// Returns `true` only if the radio was actually power-cycled, so the
/// caller can reset its miss-counter/backoff only when something was done.
/// Returns `false` on non-Windows platforms and on any path that did not
/// toggle the radio (no adapter, radio already off, denied, error) — the
/// caller then falls through to its normal backoff-with-sleep instead of
/// spinning. This keeps the Windows-specific recovery from bleeding into
/// the macOS/Linux backoff cadence (where it is always a no-op).
pub async fn try_reset_radio(clock: &dyn Clock) -> bool {
    #[cfg(target_os = "windows")]
    {
        match winrt_radio_cycle(clock).await {
            Ok(cycled) => cycled,
            Err(e) => {
                warn!("radio reset failed: {e}");
                false
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = clock;
        false
    }
}

#[cfg(target_os = "windows")]
async fn winrt_radio_cycle(clock: &dyn Clock) -> windows::core::Result<bool> {
    use windows::Devices::Bluetooth::BluetoothAdapter;
    use windows::Devices::Radios::{RadioAccessStatus, RadioState};

    let adapter = match BluetoothAdapter::GetDefaultAsync()?.await {
        Ok(adapter) => adapter,
        // A null result (no adapter) comes back as an empty error.
        Err(e) if e.code().is_ok() => {
            warn!("radio reset: no BT adapter found, skipping");
            return Ok(false);
        }
        Err(e) => return Err(e),
    };
    let radio = adapter.GetRadioAsync()?.await?;
    let state = radio.State()?;
    if state != RadioState::On {
        // Radio is off (user likely turned BT off deliberately). Don't
        // block waiting for it — return immediately. We return false, so
        // the caller does NOT update last_reset_ts and the cooldown does
        // not engage on this path; what prevents a tight loop is the
        // caller's backoff sleep on a no-reset miss. The cost while BT stays
        // off is one cheap WinRT state query per backoff cycle, which is fine.
        info!("radio reset: radio is {}, not powering — leaving it to the user", state.0);
        return Ok(false);
    }
    warn!(
        "radio reset: {RADIO_RESET_AFTER_MISSES} consecutive scan misses — power-cycling the BT \
         radio to recover. This briefly disconnects ALL Bluetooth devices \
         (mouse, keyboard, headphones), not just the eve."
    );
    let status = radio.SetStateAsync(RadioState::Off)?.await?;
    if status != RadioAccessStatus::Allowed {
        warn!("radio reset: could not turn radio off (status={}) — toggle BT manually", status.0);
        return Ok(false);
    }
    clock.sleep(2.0).await;
    let status = radio.SetStateAsync(RadioState::On)?.await?;
    if status != RadioAccessStatus::Allowed {
        warn!("radio reset: could not turn radio back on (status={})", status.0);
        return Ok(false);
    }
    clock.sleep(2.0).await;
    info!("radio reset: BT radio cycled successfully");
    Ok(true)
}

/// Why a payload could not be chunked.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ChunkError {
    /// Python's `ValueError("max_size must be positive")`.
    #[error("max_size must be positive")]
    MaxSize,
    /// `codecs.getincrementaldecoder` has no such codec.
    #[error(transparent)]
    Lookup(#[from] LookupError),
}

/// Split `data` (from `protocol::encode` with this `codec`) without ending a chunk inside a character.
///
/// UTF-8 is split by its continuation bytes. The CJK codecs (gbk, big5, shift_jis) are not: their second
/// byte can be any of 0x40-0xFE, ASCII included, so the UTF-8 rule cut a double-byte character across two
/// writes. For those, a character ends where the codec's incremental decoder holds no pending byte.
pub fn codec_safe_chunks<'a>(
    data: &'a [u8],
    max_size: usize,
    codec: Option<&str>,
) -> Result<Vec<&'a [u8]>, ChunkError> {
    let codec = match codec {
        None | Some("utf-8") => return utf8_safe_chunks(data, max_size),
        Some(name) => name,
    };
    if max_size == 0 {
        return Err(ChunkError::MaxSize);
    }
    let mut decoder = IncrementalDecoder::new(Codec::lookup(codec)?);
    let mut chunks = Vec::new();
    let (mut start, mut boundary) = (0usize, 0usize);
    for (i, &b) in data.iter().enumerate() {
        decoder.feed(b);
        if decoder.has_pending() {
            continue; // inside a character: no boundary after this byte
        }
        let end = i + 1;
        if end - start > max_size && boundary > start {
            chunks.push(&data[start..boundary]);
            start = boundary;
        }
        boundary = end;
    }
    if start < data.len() {
        chunks.push(&data[start..]);
    }
    Ok(chunks)
}

/// Split UTF-8 bytes without ending a chunk inside a codepoint.
///
/// The firmware consumes each BLE write independently, so a raw byte slice that
/// ends between the bytes of a Chinese character can render as mojibake. The
/// input comes from protocol::encode(), so it is valid UTF-8; we only need to
/// back up from continuation bytes at the proposed boundary.
pub fn utf8_safe_chunks(data: &[u8], max_size: usize) -> Result<Vec<&[u8]>, ChunkError> {
    if max_size == 0 {
        return Err(ChunkError::MaxSize);
    }
    let mut chunks = Vec::new();
    let mut offset = 0;
    while offset < data.len() {
        let mut end = (offset + max_size).min(data.len());
        if end < data.len() {
            let mut safe_end = end;
            while safe_end > offset && is_utf8_continuation(data[safe_end]) {
                safe_end -= 1;
            }
            end = if safe_end > offset {
                safe_end
            } else {
                (offset + utf8_codepoint_size(data[offset])).min(data.len())
            };
        }
        chunks.push(&data[offset..end]);
        offset = end;
    }
    Ok(chunks)
}

fn is_utf8_continuation(byte: u8) -> bool {
    (byte & 0b1100_0000) == 0b1000_0000
}

fn utf8_codepoint_size(lead_byte: u8) -> usize {
    if (lead_byte & 0b1000_0000) == 0 {
        return 1;
    }
    if (lead_byte & 0b1110_0000) == 0b1100_0000 {
        return 2;
    }
    if (lead_byte & 0b1111_0000) == 0b1110_0000 {
        return 3;
    }
    if (lead_byte & 0b1111_1000) == 0b1111_0000 {
        return 4;
    }
    1
}

#[cfg(test)]
mod tests;
