//! qr_svg against segno 1.6.6 itself: each case's `uri` is what
//! `segno.make(url, error="m").svg_data_uri(scale=1, border=4, dark="#000", light="#fff", omitsize=True)` returned.
//! The first four are pairing links as `_pair_open` draws them; the rest cover the numeric, alphanumeric and byte
//! modes (latin-1 and UTF-8) from version 1 to version 20 and more.

use serde_json::Value;

#[test]
fn qr_svg_is_byte_for_byte_segnos() {
    let cases: Value = serde_json::from_str(include_str!("qr_cases.json")).unwrap();
    let cases = cases.as_array().unwrap();
    assert!(cases.len() >= 20);
    for case in cases {
        let url = case["url"].as_str().unwrap();
        assert_eq!(eve_lite_web::qr_svg(url), case["uri"].as_str().unwrap(), "{url:?}");
    }
}

#[test]
fn a_link_too_long_for_any_qr_code_has_none() {
    // version 40 at level M holds 2331 bytes
    assert_eq!(eve_lite_web::qr_svg(&"\u{e9}x".repeat(2000)), "");
    assert!(!eve_lite_web::qr_svg("https://test-mac.local:48781/#p=abc").is_empty()); // positive control
}
