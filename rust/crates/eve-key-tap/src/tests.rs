//! Port of bridge/tests/test_key_tap.py.
//!
//! KeyTapper — the swipe-to-key path. A swipe on the pet asks the host to
//! press one key. The allowlist is the whole security story here, and the
//! event source is load-bearing: a tap built from a real HID source is
//! swallowed by Warp's global key handling.

use super::*;
use std::sync::{Arc, Mutex};

type Events = Arc<Mutex<Vec<(u16, bool)>>>;

fn fake_poster() -> (Poster, Events) {
    let events: Events = Arc::new(Mutex::new(Vec::new()));
    let e = events.clone();
    (
        Box::new(move |keycode, down| e.lock().unwrap().push((keycode, down))),
        events,
    )
}

/// The default allowlist, pinned so the developer's `EVE_ENTER_KEY` cannot
/// change what these tests see.
fn default_keys() -> HashMap<&'static str, u16> {
    tappable_with(enter_keycode_for(None))
}

fn tapper(trusted: bool) -> (KeyTapper, Events) {
    let (p, events) = fake_poster();
    let check: Option<TrustCheck> = if trusted {
        None
    } else {
        Some(Box::new(move |_prompt| trusted))
    };
    (
        KeyTapper::new(Some(p), check).with_keys(default_keys()),
        events,
    )
}

#[test]
fn test_tap_enter_presses_and_releases() {
    let (mut k, p) = tapper(true);
    assert!(k.tap_with_method("enter", None));
    assert_eq!(*p.lock().unwrap(), vec![(KEY_RETURN, true), (KEY_RETURN, false)]);
}

#[test]
fn test_tap_refuses_unknown_key() {
    let (mut k, p) = tapper(true);
    assert!(!k.tap_with_method("rm-rf", None));
    assert!(p.lock().unwrap().is_empty());
}

#[test]
fn test_tap_next_prev_map_to_arrows() {
    let (mut k, p) = tapper(true);
    assert!(k.tap_with_method("next", None));
    assert!(k.tap_with_method("prev", None));
    assert_eq!(
        *p.lock().unwrap(),
        vec![
            (KEY_DOWN_ARROW, true),
            (KEY_DOWN_ARROW, false),
            (KEY_UP_ARROW, true),
            (KEY_UP_ARROW, false),
        ]
    );
}

#[test]
fn test_tap_untrusted_posts_nothing() {
    let (mut k, p) = tapper(false);
    assert!(!k.tap_with_method("enter", None));
    assert!(p.lock().unwrap().is_empty());
}

/// No Quartz (non-mac host, pyobjc missing): tap reports failure, never
/// raises.
#[test]
fn test_no_poster_is_a_clean_noop() {
    let mut k = KeyTapper::with_factory(None, None, || None).with_keys(default_keys());
    assert!(!k.tap_with_method("enter", None));
}

#[test]
fn test_enter_defaults_to_main_return() {
    assert_eq!(enter_keycode_for(None), KEY_RETURN);
    assert_eq!(enter_keycode_for(Some("")), KEY_RETURN);
}

#[test]
fn test_enter_can_be_switched_to_keypad() {
    assert_eq!(enter_keycode_for(Some("keypad")), KEY_KEYPAD_ENTER);
    assert_eq!(enter_keycode_for(Some(" Keypad-Enter ")), KEY_KEYPAD_ENTER);
    assert_eq!(enter_keycode_for(Some("enter")), KEY_KEYPAD_ENTER);
    assert_eq!(enter_keycode_for(Some("return")), KEY_RETURN);
    assert_eq!(enter_keycode_for(Some("bogus")), KEY_RETURN);
}

/// Regression: Return built from a real HID source was swallowed by Warp's
/// global key handling and never reached the focused app. Taps use NULL;
/// EVE_KEY_SOURCE=hid forces the real source.
#[test]
fn test_taps_use_the_null_event_source() {
    #[derive(Debug, PartialEq, Clone, Copy)]
    struct Hid;
    let sources: Arc<Mutex<Vec<Option<Hid>>>> = Arc::new(Mutex::new(Vec::new()));

    let make = |force_hid: bool| {
        let s = sources.clone();
        poster_from(Some(Hid), force_hid, move |src: Option<&Hid>, _k, _d| {
            s.lock().unwrap().push(src.copied());
        })
    };

    assert!(!force_hid_for(None));
    let mut k = KeyTapper::new(Some(make(force_hid_for(None))), None).with_keys(default_keys());
    assert!(k.tap_with_method("enter", None));
    assert_eq!(*sources.lock().unwrap(), vec![None, None]);

    sources.lock().unwrap().clear();
    assert!(force_hid_for(Some("hid")));
    let mut k =
        KeyTapper::new(Some(make(force_hid_for(Some("hid")))), None).with_keys(default_keys());
    assert!(k.tap_with_method("enter", None));
    assert_eq!(*sources.lock().unwrap(), vec![Some(Hid), Some(Hid)]);
}

// ---- behavior the Python tests leave implicit ----

#[test]
fn trust_check_prompts_only_on_the_first_attempt() {
    let (p, events) = fake_poster();
    let prompts: Arc<Mutex<Vec<bool>>> = Arc::new(Mutex::new(Vec::new()));
    let pr = prompts.clone();
    let mut k = KeyTapper::new(
        Some(p),
        Some(Box::new(move |prompt| {
            pr.lock().unwrap().push(prompt);
            false
        })),
    )
    .with_keys(default_keys());
    assert!(!k.tap_with_method("enter", None));
    assert!(!k.tap_with_method("enter", None));
    assert_eq!(*prompts.lock().unwrap(), vec![true, false]);
    assert!(events.lock().unwrap().is_empty());
}

#[test]
fn a_passed_trust_check_is_not_asked_again() {
    let (p, events) = fake_poster();
    let calls = Arc::new(Mutex::new(0));
    let c = calls.clone();
    let mut k = KeyTapper::new(
        Some(p),
        Some(Box::new(move |_| {
            *c.lock().unwrap() += 1;
            true
        })),
    )
    .with_keys(default_keys());
    assert!(k.tap_with_method("enter", None));
    assert!(k.tap_with_method("next", None));
    assert_eq!(*calls.lock().unwrap(), 1);
    assert_eq!(events.lock().unwrap().len(), 4);
}

#[test]
fn a_built_poster_gets_the_real_trust_check() {
    let (p, _events) = fake_poster();
    let k = KeyTapper::with_factory(None, None, move || Some(p));
    assert!(k.trust_check.is_some());
    let (p, _events) = fake_poster();
    let k = KeyTapper::new(Some(p), None);
    assert!(k.trust_check.is_none());
}

#[test]
fn unknown_method_falls_through_to_the_poster() {
    let (mut k, p) = tapper(true);
    assert!(k.tap_with_method("prev", Some("auto")));
    assert!(k.tap_with_method("prev", Some("  QUARTZ ")));
    assert_eq!(p.lock().unwrap().len(), 4);
}

#[test]
fn keypad_enter_allowlist() {
    let (p, events) = fake_poster();
    let mut k = KeyTapper::new(Some(p), None).with_keys(tappable_with(KEY_KEYPAD_ENTER));
    assert!(k.tap_with_method("enter", None));
    assert_eq!(
        *events.lock().unwrap(),
        vec![(KEY_KEYPAD_ENTER, true), (KEY_KEYPAD_ENTER, false)]
    );
}

#[test]
fn flag_masks_match_quartz() {
    assert_eq!(FLAG_ALTERNATE, 0x00080000);
    assert_eq!(FLAG_SECONDARY_FN, 0x00800000);
}

#[test]
fn signature_kind_reads_stderr_first() {
    assert_eq!(signature_kind("", "Signature=adhoc\n"), "ad-hoc / linker-signed");
    assert_eq!(signature_kind("", "flags=0x20002(adhoc,linker-signed)"), "ad-hoc / linker-signed");
    assert_eq!(signature_kind("adhoc", "Authority=Developer ID"), "signed");
    assert_eq!(signature_kind("adhoc", ""), "ad-hoc / linker-signed");
}

#[test]
fn py_repr_matches_python() {
    assert_eq!(py_repr("rm-rf"), "'rm-rf'");
    assert_eq!(py_repr("it's"), "\"it's\"");
    assert_eq!(py_repr("a'\"b"), "'a\\'\"b'");
    assert_eq!(py_repr("x\ny\\"), "'x\\ny\\\\'");
}

#[test]
fn timeout_message_matches_python() {
    let argv = vec![
        "osascript".to_string(),
        "-e".to_string(),
        "tell application \"System Events\" to key code 36".to_string(),
    ];
    assert_eq!(
        run_error_text(&argv, Duration::from_secs(5), &RunError::Timeout),
        "Command '['osascript', '-e', 'tell application \"System Events\" to key code 36']' \
         timed out after 5 seconds"
    );
}

#[test]
fn run_with_timeout_captures_and_kills() {
    let out = run_with_timeout(
        &["sh".into(), "-c".into(), "echo hi; echo err >&2; exit 3".into()],
        Duration::from_secs(5),
    )
    .unwrap();
    assert_eq!(out.stdout, b"hi\n");
    assert_eq!(out.stderr, b"err\n");
    assert_eq!(out.status.code(), Some(3));

    let t = Instant::now();
    let r = run_with_timeout(&["sleep".into(), "5".into()], Duration::from_millis(100));
    assert!(matches!(r, Err(RunError::Timeout)));
    assert!(t.elapsed() < Duration::from_secs(3));

    let r = run_with_timeout(&["/nonexistent/eve-key-tap".into()], Duration::from_secs(1));
    let Err(e) = r else { panic!("spawned a missing binary") };
    assert_eq!(
        run_error_text(&["/nonexistent/eve-key-tap".into()], Duration::from_secs(1), &e),
        "[Errno 2] No such file or directory: '/nonexistent/eve-key-tap'"
    );
}

#[test]
fn strip_200_counts_characters() {
    let s = format!("  {}  ", "é".repeat(300));
    assert_eq!(strip_200(&s).chars().count(), 200);
}
