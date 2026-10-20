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

#[test]
fn py_repr_escapes_what_python_does_not_print() {
    assert_eq!(
        py_repr("a\u{85}\u{a0}\u{200b}\u{2028}\u{e000}\u{e0001}\u{fffe}\u{1fffe}\u{e9}"),
        "'a\\x85\\xa0\\u200b\\u2028\\ue000\\U000e0001\\ufffe\\U0001fffe\u{e9}'"
    );
    assert_eq!(py_repr("\x1b[A"), "'\\x1b[A'");
}

#[test]
fn py_text_matches_python_decode_errors() {
    let cases: [(&[u8], &str); 7] = [
        (b"\xff", "'utf-8' codec can't decode byte 0xff in position 0: invalid start byte"),
        (b"a\xe2\x82", "'utf-8' codec can't decode bytes in position 1-2: unexpected end of data"),
        (b"\xe0A", "'utf-8' codec can't decode byte 0xe0 in position 0: invalid continuation byte"),
        (b"\xed\xa0\x80", "'utf-8' codec can't decode byte 0xed in position 0: invalid continuation byte"),
        (b"\xc0\x80", "'utf-8' codec can't decode byte 0xc0 in position 0: invalid start byte"),
        (b"ok\xf0\x9f", "'utf-8' codec can't decode bytes in position 2-3: unexpected end of data"),
        (b"\xe2\x82A", "'utf-8' codec can't decode bytes in position 0-1: invalid continuation byte"),
    ];
    for (bytes, want) in cases {
        assert_eq!(py_text(bytes).unwrap_err(), want);
    }
    assert_eq!(py_text(b"a\r\nb\rc").unwrap(), "a\nb\nc");
}

#[test]
fn os_error_text_matches_python() {
    let argv = vec!["osascript".to_string()];
    let e = RunError::Spawn(std::io::Error::from_raw_os_error(13));
    assert_eq!(
        run_error_text(&argv, Duration::from_secs(5), &e),
        "[Errno 13] Permission denied: 'osascript'"
    );
}

#[test]
fn strip_200_uses_python_whitespace() {
    assert_eq!(strip_200("\u{1c} x \u{1f}\n"), "x");
}

/// Python's `communicate` timeout also covers pipes a grandchild holds open.
#[test]
fn run_with_timeout_covers_held_pipes() {
    let t = Instant::now();
    let r = run_with_timeout(
        &["sh".into(), "-c".into(), "sleep 5 & exit 0".into()],
        Duration::from_millis(200),
    );
    assert!(matches!(r, Err(RunError::Timeout)));
    assert!(t.elapsed() < Duration::from_secs(3));
}

/// Python's `.strip()` also drops the separators U+001C..U+001F, which
/// Rust's `trim` keeps; env values are parsed the Python way.
#[test]
fn env_values_strip_python_whitespace() {
    assert_eq!(enter_keycode_for(Some("\u{1c}keypad\u{1f}")), KEY_KEYPAD_ENTER);
    assert!(force_hid_for(Some("\u{1d}HID ")));
    let (mut k, _p) = tapper(true);
    // "\u{1e}bogus" is not a System Events method either way; it posts.
    assert!(k.tap_with_method("enter", Some("\u{1e}bogus")));
}
