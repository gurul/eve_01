//! Ports of `bridge/tests/test_ble_radio_reset.py` and the eve-ble half of
//! `bridge/tests/test_protocol.py`, plus Rust-side coverage of the seams.
//!
//! The radio-reset tests drive `run()` with scripted fakes so the loop
//! terminates deterministically instead of running forever. No real BLE or
//! WinRT is touched.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use async_trait::async_trait;
use eve_protocol::{Codec, encode};
use futures::FutureExt;
use futures::future::BoxFuture;
use futures::stream::{BoxStream, StreamExt};
use parking_lot::Mutex;
use serde_json::{Value, json};

use super::*;

// ---- fakes ---------------------------------------------------------------

/// Stand-in for `BleakClient`.
///
/// By default it reports connected once, then false, so run()'s hold-open
/// loop sees the link 'drop' after one iteration and proceeds to the
/// post-disconnect bookkeeping.
struct FakeClient {
    checks: AtomicUsize,
    /// `is_connected` is true for this many checks; `None` = always.
    connected_checks: Option<usize>,
    mtu: usize,
    writes: Mutex<Vec<Vec<u8>>>,
    fail_writes: bool,
    notes: Mutex<Option<Vec<Vec<u8>>>>,
    disconnects: AtomicUsize,
}

impl FakeClient {
    fn dropping_after(checks: usize) -> Self {
        FakeClient {
            checks: AtomicUsize::new(0),
            connected_checks: Some(checks),
            mtu: 23,
            writes: Mutex::new(Vec::new()),
            fail_writes: false,
            notes: Mutex::new(None),
            disconnects: AtomicUsize::new(0),
        }
    }

    /// The test_protocol `_FakeBleClient`: no radio, always connected,
    /// MTU 23 (20-byte writes), records each Write Without Response.
    fn recording() -> Self {
        FakeClient { connected_checks: None, ..FakeClient::dropping_after(0) }
    }
}

#[async_trait]
impl BleClient for FakeClient {
    fn is_connected(&self) -> bool {
        let n = self.checks.fetch_add(1, Ordering::SeqCst) + 1;
        self.connected_checks.is_none_or(|limit| n <= limit)
    }

    fn mtu_size(&self) -> usize {
        self.mtu
    }

    async fn start_notify(&self, uuid: &str) -> anyhow::Result<BoxStream<'static, Vec<u8>>> {
        assert_eq!(uuid, NUS_TX_UUID);
        let notes = self.notes.lock().take().unwrap_or_default();
        Ok(futures::stream::iter(notes).boxed())
    }

    async fn write_gatt_char(&self, uuid: &str, data: &[u8], response: bool) -> anyhow::Result<()> {
        assert_eq!(uuid, NUS_RX_UUID);
        assert!(!response);
        if self.fail_writes {
            anyhow::bail!("write refused");
        }
        self.writes.lock().push(data.to_vec());
        Ok(())
    }

    async fn disconnect(&self) -> anyhow::Result<()> {
        self.disconnects.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// Drives `EveBle::run()` with scripted device finds and radio resets.
///
/// - `find_results`: what `find_device` returns in order. `None` = miss, a
///   device = a found device (which then connects through a `FakeClient`
///   that drops at once). After the list is exhausted we keep returning None.
/// - `reset_returns`: what `try_reset_radio` returns in order.
/// - Records every reset call so assertions can read the resulting cadence.
///   Stops the loop after `stop_after` iterations so it never runs forever.
struct Harness {
    find_results: Mutex<VecDeque<Option<BleDevice<()>>>>,
    reset_returns: Mutex<VecDeque<bool>>,
    reset_calls: AtomicUsize,
    miss_count: AtomicUsize,
    found_count: AtomicUsize,
    iterations: AtomicUsize,
    stop_after: usize,
    stop: Arc<AtomicBool>,
    client_checks: usize,
    client_notes: Mutex<Option<Vec<Vec<u8>>>>,
}

impl Harness {
    fn new(find_results: Vec<Option<BleDevice<()>>>, reset_returns: Vec<bool>, stop_after: usize) -> Arc<Self> {
        Arc::new(Harness {
            find_results: Mutex::new(find_results.into()),
            reset_returns: Mutex::new(reset_returns.into()),
            reset_calls: AtomicUsize::new(0),
            miss_count: AtomicUsize::new(0),
            found_count: AtomicUsize::new(0),
            iterations: AtomicUsize::new(0),
            stop_after,
            stop: Arc::new(AtomicBool::new(false)),
            client_checks: 1,
            client_notes: Mutex::new(None),
        })
    }

    fn reset_calls(&self) -> usize {
        self.reset_calls.load(Ordering::SeqCst)
    }

    fn found_count(&self) -> usize {
        self.found_count.load(Ordering::SeqCst)
    }
}

struct FakeTransport(Arc<Harness>);

#[async_trait]
impl BleTransport for FakeTransport {
    type Peripheral = ();
    type Client = FakeClient;

    async fn find_device(
        &self,
        address: Option<&str>,
        name_prefix: &str,
        timeout_secs: f64,
    ) -> anyhow::Result<Option<BleDevice<()>>> {
        assert_eq!(address, None);
        assert_eq!(name_prefix, DEFAULT_NAME_PREFIX);
        assert_eq!(timeout_secs, SCAN_TIMEOUT_SECS);
        let h = &self.0;
        let n = h.iterations.fetch_add(1, Ordering::SeqCst) + 1;
        let result = h.find_results.lock().pop_front().flatten();
        if result.is_none() {
            h.miss_count.fetch_add(1, Ordering::SeqCst);
        } else {
            h.found_count.fetch_add(1, Ordering::SeqCst);
        }
        // Stop AFTER this iteration's result is processed: once we've produced
        // `stop_after` results, arm stop so the loop exits at its next top-of-
        // loop check without processing a further phantom miss.
        if n >= h.stop_after {
            h.stop.store(true, Ordering::SeqCst);
        }
        Ok(result)
    }

    async fn connect(&self, _device: &BleDevice<()>) -> anyhow::Result<FakeClient> {
        let client = FakeClient::dropping_after(self.0.client_checks);
        *client.notes.lock() = self.0.client_notes.lock().take();
        Ok(client)
    }

    async fn try_reset_radio(&self, _clock: &dyn Clock) -> bool {
        self.0.reset_calls.fetch_add(1, Ordering::SeqCst);
        self.0.reset_returns.lock().pop_front().unwrap_or(false)
    }
}

/// `asyncio.sleep` instant+recorded, and a fake monotonic clock that
/// advances by each sleep's duration.
///
/// Advancing the clock matters for the radio-reset cooldown: the reset re-arm
/// is gated on RADIO_RESET_COOLDOWN_SECS of monotonic time, so without
/// advancing the clock the cooldown would never elapse and re-arm would never
/// fire in tests. Each recorded sleep pushes the fake clock forward, plus a
/// small fixed step per read so even zero-sleep iterations make progress.
struct FakeClock {
    sleeps: Mutex<Vec<f64>>,
    clock: Mutex<f64>,
}

impl FakeClock {
    fn new() -> Arc<Self> {
        Arc::new(FakeClock { sleeps: Mutex::new(Vec::new()), clock: Mutex::new(1000.0) })
    }

    fn sleeps(&self) -> Vec<f64> {
        self.sleeps.lock().clone()
    }
}

impl Clock for FakeClock {
    fn sleep(&self, secs: f64) -> BoxFuture<'static, ()> {
        self.sleeps.lock().push(secs);
        *self.clock.lock() += secs.max(0.0);
        tokio::task::yield_now().boxed()
    }

    fn monotonic(&self) -> f64 {
        let mut c = self.clock.lock();
        *c += 0.001; // tiny per-call advance so ordering is monotonic
        *c
    }
}

fn noop() -> IncomingHandler {
    Arc::new(|_obj| async { Ok(()) }.boxed())
}

fn make_ble(h: &Arc<Harness>, clock: &Arc<FakeClock>) -> EveBle<FakeTransport> {
    let mut ble = EveBle::with_transport(noop(), None, None, FakeTransport(h.clone()), clock.clone());
    ble.stop = h.stop.clone();
    ble
}

fn fake_device() -> BleDevice<()> {
    BleDevice { name: Some("Claude-TEST".into()), address: "AA:BB:CC:DD:EE:FF".into(), peripheral: () }
}

fn misses(n: u64) -> Vec<Option<BleDevice<()>>> {
    (0..n).map(|_| None).collect()
}

async fn drive(h: &Arc<Harness>) -> (EveBle<FakeTransport>, Arc<FakeClock>) {
    let clock = FakeClock::new();
    let ble = make_ble(h, &clock);
    ble.run().await;
    (ble, clock)
}

// ---- miss-path state machine (test_ble_radio_reset.py) --------------------

#[tokio::test]
async fn test_reset_fires_at_threshold_not_before() {
    // All misses, reset always succeeds when called.
    let h = Harness::new(vec![], vec![true], RADIO_RESET_AFTER_MISSES as usize);
    drive(&h).await;
    // Reset must not fire before RADIO_RESET_AFTER_MISSES misses accrued.
    assert_eq!(h.reset_calls(), 1, "expected exactly one reset at threshold, got {}", h.reset_calls());
}

#[tokio::test]
async fn test_no_reset_below_threshold() {
    let h = Harness::new(vec![], vec![], RADIO_RESET_AFTER_MISSES as usize - 1);
    let (_, clock) = drive(&h).await;
    assert_eq!(h.reset_calls(), 0);
    // Every miss below threshold must have slept (no busy-loop).
    let sleeps = clock.sleeps();
    assert_eq!(sleeps.len(), RADIO_RESET_AFTER_MISSES as usize - 1);
    assert!(sleeps.iter().all(|s| *s > 0.0));
}

#[tokio::test]
async fn test_failed_reset_does_not_arm_guard_and_still_sleeps() {
    // Reset returns false the first time it's eligible, then true next cycle.
    let h = Harness::new(vec![], vec![false, true], RADIO_RESET_AFTER_MISSES as usize + 2);
    let (_, clock) = drive(&h).await;
    // Reset attempted at least twice: once failing, then re-attempted because
    // the guard was never armed by the false return.
    assert!(h.reset_calls() >= 2, "failed reset should be retried, got {} calls", h.reset_calls());
    // The miss that saw a false reset must still have produced a sleep.
    let sleeps = clock.sleeps();
    assert!(!sleeps.is_empty());
    assert!(sleeps.iter().all(|s| *s > 0.0));
}

#[tokio::test]
async fn test_successful_reset_resets_backoff_to_base() {
    let h = Harness::new(vec![], vec![true], RADIO_RESET_AFTER_MISSES as usize + 1);
    let (_, clock) = drive(&h).await;
    assert_eq!(h.reset_calls(), 1);
    // After the successful reset (which itself does not sleep via the fake
    // path), the following miss sleeps at base backoff again.
    let sleeps = clock.sleeps();
    assert!(!sleeps.is_empty(), "expected at least one sleep after reset cycle");
    assert_eq!(*sleeps.last().unwrap(), RECONNECT_BACKOFF_BASE_SECS);
}

#[tokio::test]
async fn test_reset_retries_when_first_reset_does_not_help() {
    // BLOCKING-bug regression (review #1): if a successful power-cycle does NOT
    // revive the watcher and no connection happens, the reset must be RETRIED
    // on a later run of misses — not give up after one shot and strand the user.
    // Never finds the device; reset always 'succeeds' but never revives
    // discovery. Run long enough for several re-arm windows + cooldowns.
    let h = Harness::new(vec![], vec![true; 10], 80);
    drive(&h).await;
    let calls = h.reset_calls();
    assert!(
        (2..=RADIO_RESET_MAX_ATTEMPTS as usize).contains(&calls),
        "reset must retry when it doesn't help (>=2) and respect the cap (<={RADIO_RESET_MAX_ATTEMPTS}); got {calls}"
    );
}

#[tokio::test]
async fn test_reset_attempts_capped() {
    // Reset must stop after RADIO_RESET_MAX_ATTEMPTS — never power-cycle the
    // user's whole radio in an unbounded loop.
    let h = Harness::new(vec![], vec![true; 50], 200);
    drive(&h).await;
    assert!(
        h.reset_calls() <= RADIO_RESET_MAX_ATTEMPTS as usize,
        "reset attempts must be capped at {RADIO_RESET_MAX_ATTEMPTS}, got {}",
        h.reset_calls()
    );
}

// ---- disconnect-reset path ------------------------------------------------

#[tokio::test]
async fn test_reset_rearmed_after_disconnect() {
    // A connection that drops must clear the once-per-cycle guard so a fresh
    // run of misses can power-cycle the radio again.
    let mut finds = misses(RADIO_RESET_AFTER_MISSES); // accrue to threshold -> reset #1
    finds.push(Some(fake_device())); // connect + immediate drop
    finds.extend(misses(RADIO_RESET_AFTER_MISSES)); // accrue again -> reset #2 (only if re-armed)
    let total = finds.len();
    let h = Harness::new(finds, vec![true, true], total);
    drive(&h).await;
    assert_eq!(h.found_count(), 1, "fake device should have been 'found' once");
    assert_eq!(
        h.reset_calls(),
        2,
        "reset must re-arm after disconnect and fire twice, got {} \
         (if 1, the reset guard stayed armed across the disconnect)",
        h.reset_calls()
    );
}

#[tokio::test]
async fn test_connected_event_cleared_after_disconnect() {
    // After a connect/drop cycle, the connected-event must be CLEAR, or the
    // daemon's wait_connected() consumer would busy-loop on the stale event.
    let h = Harness::new(vec![Some(fake_device())], vec![], 1);
    let (ble, _) = drive(&h).await;
    assert_eq!(h.found_count(), 1, "fake device should have connected once");
    assert!(
        !ble.connected_event_is_set(),
        "connected-event must be CLEAR after the link drops — if set, the \
         daemon's on-connected loop would busy-loop on the stale event (the spin)"
    );
}

// ---- chunking (test_protocol.py, the ble half) ----------------------------

#[test]
fn test_utf8_safe_chunks_do_not_split_cjk_at_boundary() {
    let data = encode(&json!({"msg": "ab你好cd"}), None).unwrap();
    let ni = "你".as_bytes();
    let first_chinese_byte = data.windows(ni.len()).position(|w| w == ni).unwrap();
    let max_size = first_chinese_byte + 1;

    let chunks = utf8_safe_chunks(&data, max_size).unwrap();

    assert_eq!(chunks.concat(), data);
    assert!(chunks.iter().all(|c| std::str::from_utf8(c).is_ok_and(|s| !s.is_empty())));
    assert!(chunks.iter().all(|c| c.len() <= max_size));
}

#[test]
fn test_utf8_safe_chunks_keeps_codepoint_when_max_size_is_tiny() {
    let data = "你".as_bytes();
    let chunks = utf8_safe_chunks(data, 1).unwrap();
    assert_eq!(chunks, vec![data]);
}

fn connected_ble(client: FakeClient) -> EveBle<FakeTransport> {
    let h = Harness::new(vec![], vec![], 1);
    let ble = make_ble(&h, &FakeClock::new());
    *ble.client.lock() = Some(Arc::new(client));
    ble
}

fn writes(ble: &EveBle<FakeTransport>) -> Vec<Vec<u8>> {
    ble.client.lock().as_ref().unwrap().writes.lock().clone()
}

/// Review D8: send() split CJK-codec bytes by the UTF-8 rule, whose second
/// bytes are any of 0x40-0xFE.
async fn ble_send_never_cuts_a_double_byte_character(codec: &str) {
    let ble = connected_ble(FakeClient::recording());
    let text = format!("a{}", "日本語の文字".repeat(10)); // odd offset: a pair lands on every 20-byte edge
    let data = encode(&json!({"msg": text}), Some(codec)).unwrap();
    assert!(ble.send(&json!({"msg": text}), Some(codec)).await.unwrap());
    let w = writes(&ble);
    assert_eq!(w.concat(), data);
    assert!(w.iter().all(|w| w.len() <= 20));
    // Each write is whole characters: it decodes with no pending lead byte.
    let c = Codec::lookup(codec).unwrap();
    for chunk in &w {
        let mut d = decoder::IncrementalDecoder::new(c);
        chunk.iter().for_each(|b| d.feed(*b));
        assert!(!d.has_pending(), "{codec}: write ends inside a character: {chunk:x?}");
    }
}

#[tokio::test]
async fn test_ble_send_never_cuts_a_double_byte_character_gbk() {
    ble_send_never_cuts_a_double_byte_character("gbk").await;
}

#[tokio::test]
async fn test_ble_send_never_cuts_a_double_byte_character_big5() {
    ble_send_never_cuts_a_double_byte_character("big5").await;
}

#[tokio::test]
async fn test_ble_send_never_cuts_a_double_byte_character_shift_jis() {
    ble_send_never_cuts_a_double_byte_character("shift_jis").await;
}

// ---- Rust-side coverage ---------------------------------------------------

/// Outputs of `ble._codec_safe_chunks` under CPython 3.13, recorded.
#[test]
fn codec_safe_chunks_match_python() {
    type Case = (&'static [u8], usize, Option<&'static str>, &'static [&'static [u8]]);
    let cases: &[Case] = &[
        (b"\xe4\xbd\xa0abc", 2, Some("utf8"), &[b"\xe4\xbd\xa0", b"ab", b"c"]),
        (b"\xe4\xbd\xa0abc", 2, Some("utf-8"), &[b"\xe4\xbd\xa0", b"ab", b"c"]),
        (b"\xe4\xbd\xa0abc", 2, None, &[b"\xe4\xbd\xa0", b"ab", b"c"]),
        (b"\xff\xe4\xbd\xa0\xe4a", 2, Some("UTF-8"), &[b"\xff", b"\xe4\xbd\xa0", b"\xe4a"]),
        (b"\x810ab\xb0\xa1", 2, Some("gbk"), &[b"\x810", b"ab", b"\xb0\xa1"]),
        (b"\xb1\xa1\xb1\xa1x", 3, Some("gbk"), &[b"\xb1\xa1", b"\xb1\xa1x"]),
        (b"\xb1\xa1\xb1\xa1x", 1, Some("gbk"), &[b"\xb1\xa1", b"\xb1\xa1", b"x"]),
        (b"\xb1\x05\xb1\xa1", 2, Some("gbk"), &[b"\xb1\x05", b"\xb1\xa1"]),
        (b"\xb1\xa1\xb1\xa1", 3, Some("cp936"), &[b"\xb1\xa1", b"\xb1\xa1"]),
        (b"\xb1\xa1\xb1\xa1", 3, Some("big5"), &[b"\xb1\xa1", b"\xb1\xa1"]),
        (b"\xb1\xb1\xb1\xb1", 3, Some("shift_jis"), &[b"\xb1\xb1\xb1", b"\xb1"]),
        (b"\x88\x9f\x88\x9f", 3, Some("sjis"), &[b"\x88\x9f", b"\x88\x9f"]),
        (b"\x88\x9f\x88\x9f\xeb@", 3, Some("shift_jis"), &[b"\x88\x9f", b"\x88\x9f\xeb", b"@"]),
        (b"", 3, Some("gbk"), &[]),
    ];
    for (data, max, codec, want) in cases {
        let got = codec_safe_chunks(data, *max, *codec).unwrap();
        assert_eq!(&got, want, "{data:x?} {max} {codec:?}");
    }
}

#[test]
fn codec_safe_chunks_errors_match_python() {
    assert_eq!(codec_safe_chunks(b"a", 0, Some("gbk")), Err(ChunkError::MaxSize));
    assert_eq!(codec_safe_chunks(b"a", 0, None), Err(ChunkError::MaxSize));
    assert_eq!(codec_safe_chunks(b"", 0, None), Err(ChunkError::MaxSize));
    assert_eq!(utf8_safe_chunks(b"a", 0), Err(ChunkError::MaxSize));
    let e = codec_safe_chunks(b"a", 3, Some("nope")).unwrap_err();
    assert_eq!(e.to_string(), "unknown encoding: nope");
    // The codec is looked up even for an empty payload.
    assert!(matches!(codec_safe_chunks(b"", 3, Some("nope")), Err(ChunkError::Lookup(_))));
    assert_eq!(ChunkError::MaxSize.to_string(), "max_size must be positive");
}

#[test]
fn utf8_codepoint_sizes() {
    assert_eq!(utf8_codepoint_size(b'a'), 1);
    assert_eq!(utf8_codepoint_size(0xC3), 2);
    assert_eq!(utf8_codepoint_size(0xE4), 3);
    assert_eq!(utf8_codepoint_size(0xF0), 4);
    assert_eq!(utf8_codepoint_size(0x80), 1);
    assert_eq!(utf8_codepoint_size(0xF8), 1);
    assert!(is_utf8_continuation(0x80) && is_utf8_continuation(0xBF));
    assert!(!is_utf8_continuation(0xC0) && !is_utf8_continuation(b'a'));
}

#[test]
fn decoder_tables_have_cpython_shape() {
    // gbk and big5 hold every high byte as a lead; shift_jis only 0x81-0x9F
    // and 0xE0-0xEA (0xA1-0xDF are one-byte katakana).
    for (codec, leads) in [(Codec::Gbk, 128usize), (Codec::Big5, 128), (Codec::ShiftJis, 42)] {
        let n = (0u8..=255)
            .filter(|b| {
                let mut d = decoder::IncrementalDecoder::new(codec);
                d.feed(*b);
                d.has_pending()
            })
            .count();
        assert_eq!(n, leads, "{codec:?}");
    }
    // "你好" in gbk completes; a lead followed by ASCII 0x05 does not, and
    // the ASCII byte leaves nothing pending.
    let mut d = decoder::IncrementalDecoder::new(Codec::Gbk);
    d.feed(0xC4);
    assert!(d.has_pending());
    d.feed(0xE3);
    assert!(!d.has_pending());
    d.feed(0xB1);
    d.feed(0x05);
    assert!(!d.has_pending());
    // An invalid pair whose trail is itself a lead leaves the trail pending
    // (CPython: 0x88 0x81 -> pending b"\x81"; 0xEB 0x88 -> pending b"\x88").
    for pair in [[0x88u8, 0x81], [0xEB, 0x88]] {
        let mut d = decoder::IncrementalDecoder::new(Codec::ShiftJis);
        pair.iter().for_each(|b| d.feed(*b));
        assert!(d.has_pending(), "{pair:x?}");
    }
    let mut d = decoder::IncrementalDecoder::new(Codec::ShiftJis);
    d.feed(0x88);
    d.feed(0x9F); // 0x889F is 亜
    assert!(!d.has_pending());
}

#[test]
fn utf8_decoder_holds_only_valid_prefixes() {
    let pend = |bytes: &[u8]| {
        let mut d = decoder::IncrementalDecoder::new(Codec::Utf8);
        bytes.iter().for_each(|b| d.feed(*b));
        d.has_pending()
    };
    assert!(pend(b"\xe4"));
    assert!(pend(b"\xe4\xbd"));
    assert!(!pend(b"\xe4\xbd\xa0"));
    assert!(!pend(b"\xc0")); // invalid start byte
    assert!(!pend(b"\xe0\x80")); // overlong: E0 needs A0-BF
    assert!(pend(b"\xe0\xa0"));
    assert!(!pend(b"\xed\xa0")); // surrogate
    assert!(!pend(b"\xf4\x90")); // past U+10FFFF
    assert!(pend(b"\xf0\x90\x80"));
    assert!(pend(b"\xe4\xe4")); // the second lead restarts
}

#[tokio::test]
async fn send_returns_false_when_not_connected() {
    let h = Harness::new(vec![], vec![], 1);
    let ble = make_ble(&h, &FakeClock::new());
    assert!(!ble.connected());
    assert_eq!(ble.send(&json!({"a": 1}), None).await, Ok(false));
    let ble = connected_ble(FakeClient::dropping_after(0));
    assert_eq!(ble.send(&json!({"a": 1}), None).await, Ok(false));
    assert!(writes(&ble).is_empty());
}

#[tokio::test]
async fn send_chunks_utf8_at_mtu_minus_three() {
    let mut client = FakeClient::recording();
    client.mtu = 30;
    let ble = connected_ble(client);
    let obj = json!({"msg": "你好".repeat(20)});
    assert_eq!(ble.send(&obj, None).await, Ok(true));
    let w = writes(&ble);
    assert_eq!(w.concat(), encode(&obj, None).unwrap());
    assert!(w.iter().all(|c| c.len() <= 27 && std::str::from_utf8(c).is_ok()));
    assert_eq!(w.concat().last(), Some(&b'\n'));
}

#[tokio::test]
async fn send_never_writes_below_twenty_bytes() {
    let mut client = FakeClient::recording();
    client.mtu = 0; // max(20, mtu - 3)
    let ble = connected_ble(client);
    assert_eq!(ble.send(&json!({"msg": "x".repeat(50)}), None).await, Ok(true));
    assert!(writes(&ble).iter().take(2).all(|c| c.len() == 20));
}

#[tokio::test]
async fn send_reports_a_failed_write_as_false() {
    let mut client = FakeClient::recording();
    client.fail_writes = true;
    let ble = connected_ble(client);
    assert_eq!(ble.send(&json!({"a": 1}), None).await, Ok(false));
}

#[tokio::test]
async fn send_unknown_codec_raises_from_encode_or_fails_in_chunking() {
    let ble = connected_ble(FakeClient::recording());
    // A string reaches the encoder: LookupError escapes send, as in Python.
    let e = ble.send(&json!({"msg": "hi"}), Some("nope")).await.unwrap_err();
    assert_eq!(e.to_string(), "unknown encoding: nope");
    // No string: encode succeeds, the chunker's lookup fails inside the try.
    assert_eq!(ble.send(&json!({"n": 1}), Some("nope")).await, Ok(false));
    assert!(writes(&ble).is_empty());
}

#[tokio::test]
async fn notifications_are_assembled_and_dispatched() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let handler: IncomingHandler = Arc::new(move |obj| {
        let tx = tx.clone();
        async move {
            let crash = obj.get("crash").is_some();
            tx.send(obj).unwrap();
            if crash {
                anyhow::bail!("handler failed");
            }
            Ok(())
        }
        .boxed()
    });
    // Stop on the second (missing) find, so the first connection is held
    // until the link drops rather than cut short by stop.
    let h = Harness::new(vec![Some(fake_device())], vec![], 2);
    let mut harness = Arc::try_unwrap(h).ok().unwrap();
    harness.client_checks = 20;
    *harness.client_notes.lock() =
        Some(vec![b"{\"a\":1}\n{\"cr".to_vec(), b"ash\":1}\ngarbage\n{\"b\":".to_vec(), b"2}\n".to_vec()]);
    let h = Arc::new(harness);
    let clock = FakeClock::new();
    let mut ble = EveBle::with_transport(handler, None, None, FakeTransport(h.clone()), clock.clone());
    ble.stop = h.stop.clone();
    ble.run().await;
    // Each object is handled on its own task; collect all three.
    let mut got = Vec::new();
    for _ in 0..3 {
        let v = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await.unwrap().unwrap();
        got.push(v);
    }
    got.sort_by_key(|v| v.to_string());
    assert_eq!(got, vec![json!({"a": 1}), json!({"b": 2}), json!({"crash": 1})]);
    // The hold-open loop slept once a second while the link was up (20
    // connected checks), the post-disconnect wait used the base backoff, and
    // the miss after it the doubled one.
    let sleeps = clock.sleeps();
    assert_eq!(&sleeps[..20], &[1.0; 20]);
    assert_eq!(&sleeps[20..], &[RECONNECT_BACKOFF_BASE_SECS, RECONNECT_BACKOFF_BASE_SECS * 2.0]);
}

#[tokio::test]
async fn wait_connected_wakes_on_connect() {
    let h = Harness::new(vec![], vec![], 1);
    let ble = Arc::new(make_ble(&h, &FakeClock::new()));
    let waiter = tokio::spawn({
        let ble = ble.clone();
        async move { ble.wait_connected().await }
    });
    tokio::task::yield_now().await;
    assert!(!waiter.is_finished());
    ble.connected_evt.send_replace(true);
    waiter.await.unwrap();
}

#[tokio::test]
async fn stop_sets_flag_and_disconnects_a_live_client() {
    let ble = connected_ble(FakeClient::recording());
    ble.stop().await;
    assert!(ble.stopped());
    let client = ble.client.lock().clone().unwrap();
    assert_eq!(client.disconnects.load(Ordering::SeqCst), 1);
    // A stopped loop does not run at all.
    ble.run().await;
}

#[tokio::test]
async fn connect_error_is_logged_and_backs_off() {
    struct Failing(Arc<Harness>);
    #[async_trait]
    impl BleTransport for Failing {
        type Peripheral = ();
        type Client = FakeClient;
        async fn find_device(&self, a: Option<&str>, p: &str, t: f64) -> anyhow::Result<Option<BleDevice<()>>> {
            FakeTransport(self.0.clone()).find_device(a, p, t).await
        }
        async fn connect(&self, _device: &BleDevice<()>) -> anyhow::Result<FakeClient> {
            anyhow::bail!("connect refused")
        }
    }
    let h = Harness::new(vec![Some(fake_device()), Some(fake_device())], vec![], 2);
    let clock = FakeClock::new();
    let mut ble = EveBle::with_transport(noop(), None, None, Failing(h.clone()), clock.clone());
    ble.stop = h.stop.clone();
    ble.run().await;
    // First failure waits the base backoff; the second pass is stopped, so
    // no wait follows it.
    assert_eq!(clock.sleeps(), vec![RECONNECT_BACKOFF_BASE_SECS]);
    assert!(!ble.connected());
    assert!(!ble.connected_event_is_set());
}

#[tokio::test]
async fn default_try_reset_radio_is_a_no_op_off_windows() {
    let clock = FakeClock::new();
    if cfg!(target_os = "windows") {
        return;
    }
    assert!(!try_reset_radio(clock.as_ref()).await);
    assert!(clock.sleeps().is_empty());
}

/// The daemon spawns `run()` and calls `send()` from other tasks, so both
/// futures must be `Send` on the real transport.
#[tokio::test]
async fn real_transport_futures_are_send() {
    fn require_send<F: std::future::Future + Send>(_: &F) {}
    let ble = EveBle::new(noop(), None, Some("AA:BB:CC:DD:EE:FF"));
    assert_eq!(ble.name_prefix, DEFAULT_NAME_PREFIX);
    assert_eq!(ble.address.as_deref(), Some("AA:BB:CC:DD:EE:FF"));
    assert!(!ble.connected());
    let run = ble.run();
    require_send(&run);
    drop(run);
    let obj = json!({});
    let send = ble.send(&obj, None);
    require_send(&send);
    assert_eq!(send.await, Ok(false));
}

#[test]
fn constants_match_python() {
    assert_eq!(DEFAULT_NAME_PREFIX, "Claude");
    assert_eq!(SCAN_TIMEOUT_SECS, if cfg!(target_os = "windows") { 3.0 } else { 10.0 });
    assert_eq!(RECONNECT_BACKOFF_BASE_SECS, 3.0);
    assert_eq!(RECONNECT_BACKOFF_MAX_SECS, 60.0);
    assert_eq!(STABLE_CONNECTION_SECS, 30.0);
    assert_eq!(RADIO_RESET_AFTER_MISSES, 5);
    assert_eq!(RADIO_RESET_MAX_ATTEMPTS, 3);
    assert_eq!(RADIO_RESET_COOLDOWN_SECS, 120.0);
    assert_eq!(CONNECT_TIMEOUT_SECS, 10.0);
}

/// Differential check against the bridge's own `_codec_safe_chunks` on random
/// payloads in every codec setting. Needs `EVE_BLE_PYTHON` (an interpreter
/// that can import `eve_bridge.ble`, i.e. one with bleak installed).
#[test]
#[ignore = "live: runs the bridge's Python (EVE_BLE_PYTHON)"]
fn differential_against_python_ble() {
    use std::io::Write;
    let python = std::env::var("EVE_BLE_PYTHON").expect("EVE_BLE_PYTHON");
    let src = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/src");
    let mut seed: u64 = 0x2026_1019;
    let mut rnd = move |n: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % n
    };
    let codecs = [None, Some("utf-8"), Some("utf8"), Some("gbk"), Some("big5"), Some("shift_jis"), Some("sjis")];
    let mut cases = Vec::new();
    for i in 0..3000 {
        let codec = codecs[i % codecs.len()];
        let len = rnd(48) as usize;
        let data: Vec<u8> = if i % 2 == 0 {
            (0..len).map(|_| rnd(256) as u8).collect()
        } else {
            let text: String = (0..len)
                .map(|_| char::from_u32(0x20 + rnd(0x9FA0) as u32).unwrap_or('?'))
                .collect();
            encode(&json!({"msg": text}), codec).unwrap()
        };
        let max = 1 + rnd(24) as usize;
        cases.push((data, max, codec));
    }
    let payload: Vec<Value> = cases
        .iter()
        .map(|(d, m, c)| json!([d.iter().map(|b| format!("{b:02x}")).collect::<String>(), m, c]))
        .collect();
    let script = r#"
import json, sys
from eve_bridge.ble import _codec_safe_chunks
out = []
for hexdata, m, c in json.load(sys.stdin):
    out.append([len(x) for x in _codec_safe_chunks(bytes.fromhex(hexdata), m, c)])
json.dump(out, sys.stdout)
"#;
    let mut child = std::process::Command::new(python)
        .args(["-c", script])
        .env("PYTHONPATH", src)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(Value::from(payload).to_string().as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let want: Vec<Vec<usize>> = serde_json::from_slice(&out.stdout).unwrap();
    for ((data, max, codec), want) in cases.iter().zip(want) {
        let got: Vec<usize> = codec_safe_chunks(data, *max, *codec).unwrap().iter().map(|c| c.len()).collect();
        assert_eq!(got, want, "{data:x?} {max} {codec:?}");
    }
}
