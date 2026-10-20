//! Port of bridge/tests/test_listen_key.py — the debounced Option-hold
//! tracker. No Quartz here: the tracker is fed raw flag ints. The daemon wire
//! edge tests (`Daemon._on_listen_key`, `Daemon._reset_listen`) belong to the
//! daemon crate.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::*;
use tokio::sync::mpsc::unbounded_channel;

const OTHER_FLAGS: u64 = 0x0002_0000; // kCGEventFlagMaskShift — must never count as Option

fn tracker(key: &str) -> ListenKeyTracker {
    ListenKeyTracker::for_key(key).expect("known key")
}

fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-6 * b.abs().max(1e-12)
}

// ---- press / release --------------------------------------------------------

#[test]
fn test_press_then_release_reports_both_edges() {
    let mut t = tracker("option");
    assert_eq!(t.feed(FLAG_ALTERNATE, 0.0), Some(true));
    assert_eq!(t.feed(0, 1.0), Some(false));
}

#[test]
fn test_same_state_is_never_reported_twice() {
    let mut t = tracker("option");
    assert_eq!(t.feed(FLAG_ALTERNATE, 0.0), Some(true));
    assert_eq!(t.feed(FLAG_ALTERNATE, 1.0), None);
    assert_eq!(t.feed(FLAG_ALTERNATE | OTHER_FLAGS, 2.0), None); // shift added, option still down
}

#[test]
fn test_other_modifiers_are_ignored() {
    let mut t = tracker("option");
    assert_eq!(t.feed(OTHER_FLAGS, 0.0), None);
    assert_eq!(t.feed(0, 1.0), None);
    assert!(!t.down());
}

#[test]
fn test_fn_key_uses_secondary_fn_mask() {
    let mut t = tracker("fn");
    assert_eq!(t.feed(FLAG_ALTERNATE, 0.0), None);
    assert_eq!(t.feed(FLAG_SECONDARY_FN, 1.0), Some(true));
    assert_eq!(t.feed(0, 2.0), Some(false));
}

// ---- debounce ---------------------------------------------------------------

/// A quick tap must still end with an 'up' — losing it sticks the pose.
#[test]
fn test_release_inside_window_is_held_then_flushed() {
    let mut t = tracker("option");
    assert_eq!(t.feed(FLAG_ALTERNATE, 0.0), Some(true));
    assert_eq!(t.feed(0, 0.05), None);
    assert!(approx(t.due_in(0.05).expect("pending"), DEBOUNCE_SECS - 0.05));
    assert_eq!(t.flush(0.10), None); // window still open
    assert_eq!(t.flush(DEBOUNCE_SECS), Some(false)); // released at the window's end
    assert_eq!(t.due_in(DEBOUNCE_SECS), None);
}

#[test]
fn test_chatter_that_settles_back_is_dropped() {
    let mut t = tracker("option");
    assert_eq!(t.feed(FLAG_ALTERNATE, 0.0), Some(true));
    assert_eq!(t.feed(0, 0.02), None);
    assert_eq!(t.feed(FLAG_ALTERNATE, 0.04), None); // back to what we reported
    assert_eq!(t.due_in(0.04), None);
    assert_eq!(t.flush(1.0), None);
    assert!(t.down());
}

#[test]
fn test_at_most_one_edge_per_window() {
    let mut t = tracker("option");
    let flags = [FLAG_ALTERNATE, 0].repeat(5); // 0.09 s of bounce
    let edges: Vec<Option<bool>> = flags
        .iter()
        .enumerate()
        .map(|(i, f)| t.feed(*f, i as f64 * 0.01))
        .collect();
    let reported: Vec<bool> = edges.into_iter().flatten().collect();
    assert_eq!(reported, vec![true]);
    assert_eq!(t.flush(DEBOUNCE_SECS), Some(false));
}

#[test]
fn test_edge_after_window_passes_immediately() {
    let mut t = tracker("option");
    assert_eq!(t.feed(FLAG_ALTERNATE, 0.0), Some(true));
    assert_eq!(t.feed(0, DEBOUNCE_SECS), Some(false));
}

// ---- env selection / off ----------------------------------------------------

#[test]
fn test_env_selects_fn() {
    let key = configured_listen_key_from(Some(" FN "), "darwin");
    assert_eq!(key, "fn");
    assert_eq!(tracker(&key).mask, FLAG_SECONDARY_FN);
}

#[test]
fn test_env_default_is_option_on_darwin() {
    assert_eq!(configured_listen_key_from(None, "darwin"), "option");
    assert_eq!(configured_listen_key_from(None, "linux"), "off");
}

#[test]
fn test_env_unknown_falls_back_to_default() {
    assert_eq!(configured_listen_key_from(Some("hyper"), "darwin"), "option");
}

#[test]
fn test_off_mode_reports_nothing() {
    let mut t = tracker("off");
    assert!(!t.enabled());
    assert_eq!(t.feed(FLAG_ALTERNATE, 0.0), None);
    assert_eq!(t.feed(0, 1.0), None);
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime")
}

/// Disabled must short-circuit before any platform code runs.
#[test]
fn test_start_returns_none_when_off() {
    let rt = runtime();
    let calls: Arc<Mutex<Vec<bool>>> = Arc::default();
    let sink = calls.clone();
    let off = tracker(&configured_listen_key_from(Some("off"), "darwin"));
    let stop = start_listen_key_on(move |on| sink.lock().unwrap().push(on), rt.handle(), Some(off), "darwin");
    assert!(stop.is_none());
    assert!(calls.lock().unwrap().is_empty());
}

#[test]
fn test_start_returns_none_off_darwin() {
    let rt = runtime();
    let stop = start_listen_key_on(|_on| {}, rt.handle(), Some(tracker("option")), "linux");
    assert!(stop.is_none());
}

// ---- beyond the Python file -------------------------------------------------

#[test]
fn empty_env_value_means_the_default() {
    // `os.environ.get(...) or default`: "" is falsy.
    assert_eq!(configured_listen_key_from(Some(""), "darwin"), "option");
    assert_eq!(configured_listen_key_from(Some(""), "linux"), "off");
}

#[test]
fn whitespace_only_env_value_is_unknown_and_falls_back() {
    assert_eq!(configured_listen_key_from(Some("   "), "darwin"), "option");
    assert_eq!(configured_listen_key_from(Some("\u{1f}OFF\t"), "darwin"), "off");
}

#[test]
fn unknown_key_off_darwin_falls_back_to_off() {
    assert_eq!(configured_listen_key_from(Some("hyper"), "linux"), "off");
    assert_eq!(configured_listen_key_from(Some("Option"), "linux"), "option");
}

#[test]
fn tracker_rejects_unknown_key_with_python_message() {
    let err = ListenKeyTracker::for_key("hyper").unwrap_err();
    assert_eq!(err.to_string(), "unknown listen key 'hyper'");
    // The tracker does not strip or lowercase its argument.
    assert!(ListenKeyTracker::for_key("Option").is_err());
    assert!(ListenKeyTracker::for_key(" fn").is_err());
}

#[test]
fn listen_keys_table_matches_python() {
    let names: Vec<&str> = LISTEN_KEYS.iter().map(|(k, _)| *k).collect();
    assert_eq!(names, ["option", "fn", "off"]);
    assert_eq!(listen_key_mask("option"), Some(0x0008_0000));
    assert_eq!(listen_key_mask("fn"), Some(0x0080_0000));
    assert_eq!(listen_key_mask("off"), Some(0));
    assert_eq!(listen_key_mask("hyper"), None);
    assert!(approx(DEBOUNCE_SECS, 0.15));
}

#[test]
fn default_listen_key_follows_the_build() {
    let expected = if cfg!(target_os = "macos") { "option" } else { "off" };
    assert_eq!(default_listen_key(), expected);
    assert_eq!(platform() == "darwin", cfg!(target_os = "macos"));
}

#[test]
fn first_edge_always_passes_and_due_in_none_when_idle() {
    let mut t = tracker("option");
    assert_eq!(t.due_in(0.0), None);
    assert_eq!(t.flush(0.0), None);
    // A huge negative clock is still past -inf.
    assert_eq!(t.feed(FLAG_ALTERNATE, -1e9), Some(true));
}

#[test]
fn due_in_never_negative() {
    let mut t = tracker("option");
    t.feed(FLAG_ALTERNATE, 0.0);
    t.feed(0, 0.01);
    assert_eq!(t.due_in(10.0), Some(0.0));
}

#[test]
fn custom_debounce_is_used() {
    let mut t = ListenKeyTracker::new(Some("option"), 1.0).unwrap();
    assert_eq!(t.feed(FLAG_ALTERNATE, 0.0), Some(true));
    assert_eq!(t.feed(0, 0.5), None);
    assert_eq!(t.flush(0.99), None);
    assert_eq!(t.flush(1.0), Some(false));
}

#[test]
fn py_repr_matches_python() {
    assert_eq!(py_repr("hyper"), "'hyper'");
    assert_eq!(py_repr("it's"), "\"it's\"");
    assert_eq!(py_repr("a'b\"c"), "'a\\'b\"c'");
    assert_eq!(py_repr("a\\b"), "'a\\\\b'");
    assert_eq!(py_repr("a\nb\tc"), "'a\\nb\\tc'");
    assert_eq!(py_repr("x\u{7}y"), "'x\\x07y'");
    assert_eq!(py_repr("a\u{a0}b"), "'a\\xa0b'");
    assert_eq!(py_repr("a\u{200b}b"), "'a\\u200bb'");
    assert_eq!(py_repr("é ü"), "'é ü'");
    assert_eq!(py_repr(""), "''");
}

// ---- the loop half (Python's _feed / _flush on the event loop) ------------

fn edges() -> (Arc<Mutex<Vec<bool>>>, impl FnMut(bool) + Send + 'static) {
    let seen: Arc<Mutex<Vec<bool>>> = Arc::default();
    let sink = seen.clone();
    (seen, move |on| sink.lock().unwrap().push(on))
}

#[tokio::test(start_paused = true)]
async fn driver_emits_edges_and_flushes_a_held_release() {
    let (tx, rx) = unbounded_channel();
    let (seen, on_change) = edges();
    let task = tokio::spawn(drive_tracker(rx, tracker("option"), on_change));

    tx.send(FLAG_ALTERNATE).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(*seen.lock().unwrap(), [true]);

    tx.send(0).unwrap(); // released inside the window: held back
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(*seen.lock().unwrap(), [true]);

    tokio::time::sleep(Duration::from_millis(200)).await; // window closes
    assert_eq!(*seen.lock().unwrap(), [true, false]);

    drop(tx);
    task.await.unwrap();
    assert_eq!(*seen.lock().unwrap(), [true, false]);
}

#[tokio::test(start_paused = true)]
async fn driver_drops_chatter_that_settles_back() {
    let (tx, rx) = unbounded_channel();
    let (seen, on_change) = edges();
    let task = tokio::spawn(drive_tracker(rx, tracker("option"), on_change));

    tx.send(FLAG_ALTERNATE).unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    tx.send(0).unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    tx.send(FLAG_ALTERNATE).unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(*seen.lock().unwrap(), [true]);

    drop(tx);
    task.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn driver_releases_pending_edge_after_the_sender_closes() {
    let (tx, rx) = unbounded_channel();
    let (seen, on_change) = edges();
    let task = tokio::spawn(drive_tracker(rx, tracker("option"), on_change));

    tx.send(FLAG_ALTERNATE).unwrap();
    tx.send(0).unwrap();
    drop(tx); // stopped mid-window: the armed flush still fires
    task.await.unwrap();
    assert_eq!(*seen.lock().unwrap(), [true, false]);
}

#[tokio::test(start_paused = true)]
async fn driver_bounce_storm_ends_in_the_settled_state() {
    let (tx, rx) = unbounded_channel();
    let (seen, on_change) = edges();
    let task = tokio::spawn(drive_tracker(rx, tracker("option"), on_change));

    for f in [FLAG_ALTERNATE, 0].repeat(5) {
        tx.send(f).unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(*seen.lock().unwrap(), [true, false]);
    drop(tx);
    task.await.unwrap();
}

#[test]
#[ignore = "live: needs macOS Input Monitoring for the test binary and a held Option key"]
fn live_tap_starts_and_stops() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (_seen, on_change) = edges();
    let stop = start_listen_key_on(on_change, rt.handle(), Some(tracker("option")), "darwin");
    let stop = stop.expect("tap created");
    std::thread::sleep(Duration::from_secs(2));
    stop();
    stop(); // twice is harmless
}
