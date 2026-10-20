//! web_auth.py: eve lite's cookies, paired devices, the pairing window and the Mac's one-time login.
//! Port of bridge/tests/test_web_auth.py.
//!
//! Fake clocks drive every expiry. Tokens, secrets and codes are the ones the code makes; a wrong one is a canary
//! that is not shaped like a key.

use super::*;
use serde_json::json;
use std::collections::HashSet;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicU64, Ordering};

const CANARY: &str = "CANARY-NOT-A-KEY-7f3c";

/// A fake clock the test moves by hand.
#[derive(Clone)]
struct FakeClock(Arc<AtomicU64>);

impl FakeClock {
    fn new() -> Self {
        FakeClock(Arc::new(AtomicU64::new(1_800_000_000f64.to_bits())))
    }
    fn t(&self) -> f64 {
        f64::from_bits(self.0.load(Ordering::SeqCst))
    }
    fn advance(&self, by: f64) {
        self.0.store((self.t() + by).to_bits(), Ordering::SeqCst);
    }
    fn clock(&self) -> Clock {
        let me = self.clone();
        Arc::new(move || me.t())
    }
}

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// `caplog`: run `f` with a thread-local subscriber at DEBUG and return the log text.
fn caplog<R>(f: impl FnOnce() -> R) -> (R, String) {
    let buf = Captured::default();
    let writer = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(move || writer.clone())
        .finish();
    let out = tracing::subscriber::with_default(subscriber, f);
    let text = String::from_utf8(buf.0.lock().clone()).unwrap();
    (out, text)
}

fn sha_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

fn re(pattern: &str) -> Regex {
    Regex::new(&format!(r"\A(?:{pattern})\z")).unwrap()
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn s(v: &str) -> Value {
    json!(v)
}

// ---- cookies --------------------------------------------------------------------------------------------

#[test]
fn test_the_cookie_names_and_their_life() {
    assert_eq!((COOKIE_MAC, COOKIE_DEV), ("eve_mac", "eve_dev"));
    assert_eq!(MAX_AGE, 34_560_000);
    assert_eq!(MAX_AGE, 400 * 86400);
}

#[test]
fn test_cookie_flags_are_exact_per_listener() {
    let token = CANARY;
    assert_eq!(
        set_cookie(COOKIE_MAC, token).unwrap(),
        format!("eve_mac={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age=34560000")
    );
    assert_eq!(
        set_cookie(COOKIE_DEV, token).unwrap(),
        format!("eve_dev={token}; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=34560000")
    );
    assert_eq!(clear_cookie(COOKIE_MAC).unwrap(), "eve_mac=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0");
    assert_eq!(clear_cookie(COOKIE_DEV).unwrap(), "eve_dev=; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=0");
    for bad_name in ["session", "eve", ""] {
        assert_eq!(set_cookie(bad_name, token), Err(CookieError::NotACookie));
        assert_eq!(clear_cookie(bad_name), Err(CookieError::NotACookie));
    }
    let long = "x".repeat(300);
    for bad_token in ["short", "has space in it here", "semi;colon-injected-here", long.as_str(), "42"] {
        assert_eq!(set_cookie(COOKIE_DEV, bad_token), Err(CookieError::NotAToken));
    }
    // the bad name is told before the bad token, as in Python
    assert_eq!(set_cookie("session", "short"), Err(CookieError::NotACookie));
    assert_eq!(CookieError::NotACookie.to_string(), "not an eve lite cookie");
    assert_eq!(CookieError::NotAToken.to_string(), "not a device token");
}

#[test]
fn test_cookie_value_reads_a_cookie_header_without_tripping() {
    let header = r#"theme=dark; broken; =nothing; eve_dev=TOKEN_one-2; xeve_dev=other; eve_mac="QUOTED_two-3""#;
    assert_eq!(cookie_value(Some(header), COOKIE_DEV), "TOKEN_one-2");
    assert_eq!(cookie_value(Some(header), COOKIE_MAC), "QUOTED_two-3");
    assert_eq!(cookie_value(Some("eve_dev=; eve_dev=second"), COOKIE_DEV), "second");
    assert_eq!(cookie_value(Some("xeve_dev=1"), COOKIE_DEV), "");
    assert_eq!(cookie_value(Some(""), COOKIE_DEV), "");
    assert_eq!(cookie_value(None, COOKIE_DEV), "");
}

#[test]
fn test_cookie_value_keeps_what_follows_the_first_equals_sign() {
    // str.partition("="): the value keeps any later "=" (base64 padding, for one)
    assert_eq!(cookie_value(Some("eve_dev=a=b"), COOKIE_DEV), "a=b");
    assert_eq!(cookie_value(Some(r#"eve_dev="""#), COOKIE_DEV), "");
    assert_eq!(cookie_value(Some(r#"eve_dev="; eve_dev=x"#), COOKIE_DEV), "\"");
}

// ---- devices --------------------------------------------------------------------------------------------

#[test]
fn test_device_tokens_are_stored_only_as_hashes_in_a_0600_file() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("web-devices.json");
    let devices = Devices::new(&path);
    let (dev_id, token) = devices.add("iPhone", Some("kitchen-mac.local:48781"), DEFAULT_DOOR).unwrap();
    assert!(re("[0-9a-f]{8}").is_match(&dev_id));
    assert!(re("[A-Za-z0-9_-]{43}").is_match(&token)); // 256 bits, cookie-safe as it is
    assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    let text = fs::read_to_string(&path).unwrap();
    assert!(!text.contains(&token));
    assert!(text.contains(&sha_hex(&token))); // control: what is kept is the hash
    let data: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(data["version"], 1);
    let ids: Vec<&str> = data["devices"].as_array().unwrap().iter().map(|d| d["id"].as_str().unwrap()).collect();
    assert_eq!(ids, [dev_id.as_str()]);
    let names: Vec<String> =
        fs::read_dir(tmp.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(names, ["web-devices.json"]); // no temporary file left behind
}

#[test]
fn test_the_file_is_written_as_python_writes_it() {
    // json.dumps({"version": 1, "devices": rows}, indent=2) + "\n", ensure_ascii on: Python reads it back as is.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("web-devices.json");
    let clock = FakeClock::new();
    let devices = Devices::with_clock(&path, clock.clock());
    let (dev_id, token) = devices.add("Guru’s iPhone 🍎", Some("mac.local"), "local").unwrap();
    let want = format!(
        "{{\n  \"version\": 1,\n  \"devices\": [\n    {{\n      \"id\": \"{dev_id}\",\n      \"label\": \
         \"Guru\\u2019s iPhone \\ud83c\\udf4e\",\n      \"host\": \"mac.local\",\n      \"door\": \"local\",\n      \
         \"sha256\": \"{}\",\n      \"created\": 1800000000,\n      \"last_seen\": 1800000000\n    }}\n  ]\n}}\n",
        sha_hex(&token)
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), want);
    assert_eq!(devices.revoke("all").unwrap(), [dev_id]);
    assert_eq!(fs::read_to_string(&path).unwrap(), "{\n  \"version\": 1,\n  \"devices\": []\n}\n");
}

#[test]
fn test_check_finds_the_device_and_nothing_else() {
    let tmp = tempfile::tempdir().unwrap();
    let devices = Devices::new(tmp.path().join("web-devices.json"));
    let (dev_id, token) = devices.add("iPhone", Some("kitchen-mac.local:48781"), DEFAULT_DOOR).unwrap();
    let found = devices.check(&token, None).unwrap();
    assert_eq!(
        (found.id.as_str(), found.label.as_str(), found.host.as_str()),
        (dev_id.as_str(), "iPhone", "kitchen-mac.local:48781")
    );
    let upper = if token.to_uppercase() != token { token.to_uppercase() } else { CANARY.to_string() };
    let short = &token[..token.len() - 1];
    let longer = format!("{token}x");
    let many = "a".repeat(300);
    // Python also refuses None and 42: a non-string never reaches this typed API
    for wrong in [CANARY, short, longer.as_str(), upper.as_str(), "", many.as_str(), "bad token with spaces in it"] {
        assert!(devices.check(wrong, None).is_none(), "{wrong}");
    }
}

#[test]
fn test_a_token_is_refused_on_the_other_door() {
    // A phone pairs on the LAN door; its token is refused when the Mac door (`local`) asks with it, so it reaches
    // no Mac-only route. The Mac's own device is refused on the LAN door the same way. With no door asked, either
    // works (the daemon's own list and revoke do not care which door a token belongs to).
    let tmp = tempfile::tempdir().unwrap();
    let devices = Devices::new(tmp.path().join("web-devices.json"));
    let (_phone_id, phone) = devices.add("iPhone", Some("kitchen-mac.local:48781"), "lan").unwrap();
    let (_mac_id, mac) = devices.add("This Mac", Some("localhost:48780"), "local").unwrap();
    assert!(devices.check(&phone, Some("lan")).is_some() && devices.check(&phone, Some("local")).is_none());
    assert!(devices.check(&mac, Some("local")).is_some() && devices.check(&mac, Some("lan")).is_none());
    assert!(devices.check(&phone, None).is_some() && devices.check(&mac, None).is_some()); // no door asked: either
}

#[test]
fn test_a_row_with_no_door_reads_as_lan_only() {
    // An older file's row, or one written by hand, has no door: it reads as `lan` (the lower-privilege door), so
    // it can never stand in for the Mac's own page.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("web-devices.json");
    let digest = sha_hex(CANARY);
    let row = json!({"id": "0a1b2c3d", "label": "old", "host": "h", "sha256": digest, "created": 1,
                     "last_seen": 1_800_000_000});
    for door in [None, Some(json!("")), Some(json!("admin")), Some(json!(1))] {
        let mut raw = row.clone();
        if let Some(d) = &door {
            raw["door"] = d.clone();
        }
        fs::write(&path, json!({"version": 1, "devices": [raw]}).to_string()).unwrap();
        let devices = Devices::with_clock(&path, FakeClock::new().clock());
        assert!(devices.check(CANARY, Some("lan")).is_some(), "{door:?}");
        assert!(devices.check(CANARY, Some("local")).is_none(), "{door:?}");
    }
}

#[test]
fn test_revoke_returns_the_ids_and_the_token_stops_working() {
    let tmp = tempfile::tempdir().unwrap();
    let devices = Devices::new(tmp.path().join("web-devices.json"));
    let (a_id, a_token) = devices.add("iPhone", Some("kitchen-mac.local:48781"), DEFAULT_DOOR).unwrap();
    let (b_id, b_token) = devices.add("Laptop", Some("192.168.1.20:48781"), DEFAULT_DOOR).unwrap();
    let (c_id, c_token) = devices.add("This Mac", Some("localhost:48780"), DEFAULT_DOOR).unwrap();
    assert_eq!(devices.revoke(&a_id).unwrap(), [a_id.as_str()]);
    assert!(devices.check(&a_token, None).is_none() && devices.check(&b_token, None).is_some());
    assert!(devices.revoke(&a_id).unwrap().is_empty());
    assert!(devices.revoke("ffffffff").unwrap().is_empty());
    assert!(devices.revoke("").unwrap().is_empty());
    assert_eq!(devices.revoke(&format!("  {} ", b_id.to_uppercase())).unwrap(), [b_id.as_str()]); // as typed
    let (new_id, _) = devices.add("Phone 2", Some("kitchen-mac.local:48781"), DEFAULT_DOOR).unwrap();
    assert!(new_id != a_id && new_id != b_id);
    let gone = devices.revoke("all").unwrap();
    assert!(gone.contains(&c_id) && gone.len() == 2);
    assert!(devices.list().is_empty() && devices.check(&c_token, None).is_none());
}

#[test]
fn test_list_shows_devices_oldest_first_without_hashes() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = FakeClock::new();
    let devices = Devices::with_clock(tmp.path().join("web-devices.json"), clock.clock());
    let (first, _t1) = devices.add("iPhone", Some("kitchen-mac.local:48781"), DEFAULT_DOOR).unwrap();
    clock.advance(60.0);
    let (second, _t2) = devices.add("Laptop", Some("192.168.1.20:48781"), DEFAULT_DOOR).unwrap();
    let listed = devices.list();
    assert_eq!(listed.iter().map(|d| d.id.clone()).collect::<Vec<_>>(), [first, second]);
    for d in &listed {
        let keys: Vec<String> = serde_json::to_value(d).unwrap().as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["id", "label", "host", "created", "last_seen"]);
    }
    assert_eq!(listed[1].created, clock.t() as i64);
}

#[test]
fn test_a_revoke_by_another_process_counts_at_once() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("web-devices.json");
    let daemon = Devices::new(&path);
    let (dev_id, token) = daemon.add("iPhone", Some("kitchen-mac.local:48781"), DEFAULT_DOOR).unwrap();
    assert!(daemon.check(&token, None).is_some());
    assert_eq!(Devices::new(&path).revoke(&dev_id).unwrap(), [dev_id]); // e.g. the CLI with the daemon down
    assert!(daemon.check(&token, None).is_none());
    fs::remove_file(&path).unwrap(); // the file deleted: every device is gone
    assert!(Devices::new(&path).list().is_empty());
}

#[test]
fn test_a_damaged_file_admits_no_one_and_is_rewritten() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("web-devices.json");
    let devices = Devices::new(&path);
    let (_id, token) = devices.add("iPhone", Some("kitchen-mac.local:48781"), DEFAULT_DOOR).unwrap();
    fs::write(&path, "{not json").unwrap();
    let (found, log) = caplog(|| devices.check(&token, None));
    assert!(found.is_none());
    assert!(log.contains("could not be read"), "{log}");
    assert!(log.contains("(JSONDecodeError)"), "{log}");
    let (new_id, new_token) = devices.add("iPhone", Some("kitchen-mac.local:48781"), DEFAULT_DOOR).unwrap();
    assert!(devices.check(&new_token, None).is_some());
    let ids: Vec<Value> = read_json(&path)["devices"].as_array().unwrap().iter().map(|d| d["id"].clone()).collect();
    assert_eq!(ids, [json!(new_id)]);
}

#[test]
fn test_malformed_rows_are_dropped_not_trusted() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("web-devices.json");
    let good = json!({"id": "0a1b2c3d", "label": "iPhone", "host": "h", "sha256": sha_hex(CANARY),
                      "created": 1, "last_seen": 1_800_000_000});
    let with = |key: &str, value: Value, id: Option<&str>| {
        let mut row = good.clone();
        row[key] = value;
        if let Some(id) = id {
            row["id"] = json!(id);
        }
        row
    };
    let rows = json!([
        good.clone(),
        with("id", json!("NOTHEX!!"), None),
        with("sha256", json!("short"), Some("1a1b2c3d")),
        with("created", json!("yesterday"), Some("2a1b2c3d")),
        with("last_seen", json!(true), Some("3a1b2c3d")),
        with("created", json!(1.0), Some("4a1b2c3d")),
        with("created", json!(-1), Some("5a1b2c3d")),
        "row",
        null,
        with("label", json!("ok"), None), // a second row with the same id: the first wins
    ]);
    fs::write(&path, json!({"version": 1, "devices": rows}).to_string()).unwrap();
    let devices = Devices::with_clock(&path, FakeClock::new().clock());
    assert_eq!(devices.list().iter().map(|d| d.id.as_str()).collect::<Vec<_>>(), ["0a1b2c3d"]);
    let found = devices.check(CANARY, None).unwrap();
    assert_eq!(found.label, "iPhone");
}

#[test]
fn test_a_file_that_is_not_a_device_list_admits_no_one() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("web-devices.json");
    for body in ["[]", "{}", "{\"devices\": {}}", "null", "\"x\""] {
        fs::write(&path, body).unwrap();
        assert!(Devices::new(&path).list().is_empty(), "{body}");
    }
    fs::write(&path, [0xffu8, 0xfe, 0x00]).unwrap();
    let (rows, log) = caplog(|| Devices::new(&path).list());
    assert!(rows.is_empty());
    assert!(log.contains("could not be read (UnicodeDecodeError)"), "{log}");
}

#[test]
fn test_a_failed_save_never_half_pairs_and_a_revoke_holds_at_once() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("web-devices.json");
    let clock = FakeClock::new();
    let full = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let full_w = full.clone();
    let devices = Devices::with_clock(&path, clock.clock()).with_writer(Arc::new(move |p: &Path, d: &[u8]| {
        if full_w.load(Ordering::SeqCst) {
            return Err(io::Error::from_raw_os_error(libc::ENOSPC)); // "No space left on device"
        }
        write_private(p, d)
    }));
    let (kept_id, kept_token) = devices.add("iPhone", Some("kitchen-mac.local:48781"), DEFAULT_DOOR).unwrap();

    full.store(true, Ordering::SeqCst);
    assert!(devices.add("Laptop", Some("192.168.1.20:48781"), DEFAULT_DOOR).is_err());
    assert_eq!(devices.list().iter().map(|d| d.id.clone()).collect::<Vec<_>>(), [kept_id.as_str()]); // nothing half paired
    clock.advance(SEEN_SECS as f64);
    let (found, log) = caplog(|| devices.check(&kept_token, None));
    assert!(found.is_some()); // a full disk signs no one out
    assert!(log.contains("could not save"), "{log}");
    assert!(log.contains("(OSError)"), "{log}");
    assert!(devices.revoke(&kept_id).is_err()); // told: it would not outlive a restart
    assert!(devices.check(&kept_token, None).is_none()); // but the token stopped working at once
}

#[test]
fn test_labels_are_one_clean_line() {
    let tmp = tempfile::tempdir().unwrap();
    let devices = Devices::new(tmp.path().join("web-devices.json"));
    let label = format!("Guest\u{202e}'s\x00 phone\n\tnow{}", "x".repeat(100));
    devices.add(&label, None, DEFAULT_DOOR).unwrap();
    let rows = devices.list();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert!(row.label.starts_with("Guest's phone now") && row.label.chars().count() == 64, "{}", row.label);
    assert!(!row.label.contains('\u{202e}') && row.host.is_empty());
}

#[test]
fn test_text_matches_python_isspace_and_categories() {
    // U+001C..U+001F are str.isspace() in Python: a gap, not a dropped control character
    assert_eq!(text(Some("a\x1cb\x1fc")), "a b c");
    assert_eq!(text(Some("  a \u{3000} b\u{2028}")), "a b");
    assert_eq!(text(Some("zero\u{200b}width\u{e000}private")), "zerowidthprivate");
    assert_eq!(text(Some("é 名前")), "é 名前");
    assert_eq!(text(None), "");
}

#[test]
fn test_last_seen_moves_without_writing_on_every_request() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = FakeClock::new();
    let path = tmp.path().join("web-devices.json");
    let devices = Devices::with_clock(&path, clock.clock());
    let (_id, token) = devices.add("iPhone", Some("kitchen-mac.local:48781"), DEFAULT_DOOR).unwrap();
    let written = fs::read(&path).unwrap();
    clock.advance(30.0);
    assert_eq!(devices.check(&token, None).unwrap().last_seen, clock.t() as i64 - 30);
    assert_eq!(fs::read(&path).unwrap(), written);
    clock.advance(SEEN_SECS as f64);
    assert_eq!(devices.check(&token, None).unwrap().last_seen, clock.t() as i64);
    assert_eq!(read_json(&path)["devices"][0]["last_seen"], json!(clock.t() as i64));
}

#[test]
fn test_a_device_unseen_for_max_age_is_refused_and_forgotten() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = FakeClock::new();
    let devices = Devices::with_clock(tmp.path().join("web-devices.json"), clock.clock());
    let (_old, old_token) = devices.add("Old phone", Some("kitchen-mac.local:48781"), DEFAULT_DOOR).unwrap();
    clock.advance((MAX_AGE - 10) as f64);
    let (_new, new_token) = devices.add("New phone", Some("kitchen-mac.local:48781"), DEFAULT_DOOR).unwrap();
    clock.advance(20.0);
    assert!(devices.check(&old_token, None).is_none() && devices.check(&new_token, None).is_some());
    assert_eq!(devices.list().iter().map(|d| d.label.as_str()).collect::<Vec<_>>(), ["New phone"]);
    devices.add("Laptop", Some("192.168.1.20:48781"), DEFAULT_DOOR).unwrap();
    let file = read_json(&tmp.path().join("web-devices.json"));
    let labels: Vec<&str> = file["devices"].as_array().unwrap().iter().map(|d| d["label"].as_str().unwrap()).collect();
    assert_eq!(labels, ["New phone", "Laptop"]);
}

#[test]
fn test_past_max_devices_the_one_seen_longest_ago_goes() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = FakeClock::new();
    let devices = Devices::with_clock(tmp.path().join("web-devices.json"), clock.clock()).with_max_devices(3);
    let mut ids_tokens = Vec::new();
    for name in ["a", "b", "c"] {
        ids_tokens.push(devices.add(name, Some("h"), DEFAULT_DOOR).unwrap());
        clock.advance(SEEN_SECS as f64);
    }
    devices.check(&ids_tokens[0].1, None); // "a" was just seen; "b" now longest ago
    let (_, log) = caplog(|| devices.add("d", Some("h"), DEFAULT_DOOR).unwrap());
    assert!(log.contains("forgot 1 device(s) seen longest ago (more than 3 paired)"), "{log}");
    assert_eq!(devices.list().iter().map(|d| d.label.as_str()).collect::<Vec<_>>(), ["a", "c", "d"]);
    assert!(devices.check(&ids_tokens[1].1, None).is_none());
}

#[test]
fn test_device_secrets_never_reach_a_log() {
    let tmp = tempfile::tempdir().unwrap();
    let ((dev_id, token), log) = caplog(|| {
        let devices = Devices::new(tmp.path().join("web-devices.json"));
        let (dev_id, token) = devices.add("iPhone", Some("kitchen-mac.local:48781"), DEFAULT_DOOR).unwrap();
        devices.check(&token, None);
        devices.revoke(&dev_id).unwrap();
        (dev_id, token)
    });
    assert!(log.contains("paired a device") && log.contains("revoked 1"), "{log}"); // control: lines were captured
    assert!(!log.contains(&token) && !log.contains(&sha_hex(&token)));
    assert!(!dev_id.is_empty());
}

#[test]
fn test_write_private_replaces_a_stale_temporary_file() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("sub").join("web-devices.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path.with_file_name(".web-devices.json.tmp"), "stale").unwrap();
    write_private(&path, b"fresh").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"fresh");
    assert!(!path.with_file_name(".web-devices.json.tmp").exists());
    assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
}

// ---- the pairing window ---------------------------------------------------------------------------------

#[test]
fn test_no_pairing_without_an_open_window() {
    let window = PairingWindow::new();
    assert!(!window.is_open() && window.remaining() == 0.0);
    assert!(!window.pair(Some(&s(CANARY)), None) && !window.pair(None, Some(&s("123456"))));
}

#[test]
fn test_a_window_holds_a_128_bit_secret_and_a_6_digit_code() {
    let window = PairingWindow::with_clock(FakeClock::new().clock());
    let (secret, code) = window.open();
    assert!(re("[A-Za-z0-9_-]{22}").is_match(&secret)); // 16 random bytes, safe in a URL fragment
    assert!(re("[0-9]{6}").is_match(&code));
    assert!(window.is_open() && window.remaining() == 300.0);
    let secrets: HashSet<String> = (0..20).map(|_| window.open().0).collect();
    assert_eq!(secrets.len(), 20);
}

#[test]
fn test_the_secret_pairs_once_and_closes_the_window() {
    let window = PairingWindow::with_clock(FakeClock::new().clock());
    let (secret, code) = window.open();
    assert!(window.pair(Some(&s(&secret)), None));
    assert!(!window.is_open());
    assert!(!window.pair(Some(&s(&secret)), None)); // single use
    assert!(!window.pair(None, Some(&s(&code)))); // the code went with it
}

#[test]
fn test_the_code_pairs_once_as_typed() {
    for typed in ["{code}", "{code3} {code_3}", "{code3}-{code_3}", " {code} ", "int"] {
        let window = PairingWindow::with_clock(FakeClock::new().clock());
        let (secret, code) = window.open();
        let given = if typed == "int" {
            json!(code.parse::<i64>().unwrap())
        } else {
            json!(typed.replace("{code}", &code).replace("{code3}", &code[..3]).replace("{code_3}", &code[3..]))
        };
        assert!(window.pair(None, Some(&given)), "{typed}");
        assert!(!window.pair(None, Some(&s(&code))) && !window.pair(Some(&s(&secret)), None), "{typed}");
    }
}

#[test]
fn test_code_digits_takes_only_a_six_digit_code() {
    assert_eq!(code_digits(&json!(42)).as_deref(), Some("000042"));
    assert_eq!(code_digits(&json!(1_000_000)), None);
    assert_eq!(code_digits(&json!(-1)), None);
    assert_eq!(code_digits(&json!(123456.0)), None); // a float, not an int
    assert_eq!(code_digits(&json!(true)), None);
    assert_eq!(code_digits(&json!("12\x1c3456")).as_deref(), Some("123456"));
    assert_eq!(code_digits(&json!("１２３４５６")), None); // [0-9] is ASCII only
    assert_eq!(code_digits(&json!(format!("123456{}", " ".repeat(27)))), None); // longer than 32
}

#[test]
fn test_a_window_expires() {
    let clock = FakeClock::new();
    let window = PairingWindow::with_clock(clock.clock());
    let (secret, code) = window.open_for(300.0);
    clock.advance(299.0);
    assert!(window.is_open() && (window.remaining() - 1.0).abs() < 1e-6);
    clock.advance(1.0);
    assert!(!window.is_open() && window.remaining() == 0.0);
    assert!(!window.pair(Some(&s(&secret)), None) && !window.pair(None, Some(&s(&code))));
}

#[test]
fn test_five_wrong_tries_close_the_window() {
    let window = PairingWindow::with_clock(FakeClock::new().clock());
    let (secret, code) = window.open();
    let wrong_code = format!("{:06}", (code.parse::<u32>().unwrap() + 1) % 1_000_000);
    for _ in 0..4 {
        assert!(!window.pair(None, Some(&s(&wrong_code))));
    }
    assert!(window.pair(Some(&s(&secret)), None)); // control: four wrong tries still allow it

    let (secret, code) = window.open();
    let wrong_code = format!("{:06}", (code.parse::<u32>().unwrap() + 1) % 1_000_000);
    let tries: Vec<(Option<Value>, Option<Value>)> = vec![
        (None, Some(s(&wrong_code))),
        (Some(s(CANARY)), None),
        (None, None),
        (Some(s(&secret)), Some(s(&code))),
        (None, Some(s("12345x"))),
    ];
    for (sec, cod) in &tries {
        // wrong, malformed, neither, both: all count
        assert!(!window.pair(sec.as_ref(), cod.as_ref()));
    }
    assert!(!window.is_open());
    assert!(!window.pair(Some(&s(&secret)), None) && !window.pair(None, Some(&s(&code))));
}

#[test]
fn test_a_non_string_secret_counts_as_a_wrong_try() {
    // body.get("secret") of 42 is a given secret in Python: with the code too, it is "both", and refused
    let window = PairingWindow::with_clock(FakeClock::new().clock());
    let (_secret, code) = window.open();
    assert!(!window.pair(Some(&json!(42)), Some(&s(&code))));
    assert!(!window.pair(Some(&json!(42)), None));
    // JSON null is no value at all: the code alone pairs
    assert!(window.pair(Some(&Value::Null), Some(&s(&code))));
}

#[test]
fn test_a_new_window_replaces_the_old_one_and_its_tries() {
    let window = PairingWindow::with_clock(FakeClock::new().clock());
    let (old_secret, _old_code) = window.open();
    for _ in 0..4 {
        window.pair(Some(&s(CANARY)), None);
    }
    let (_new_secret, new_code) = window.open();
    assert!(!window.pair(Some(&s(&old_secret)), None));
    for _ in 0..3 {
        // the count started again: 1 + 3 < 5
        window.pair(Some(&s(CANARY)), None);
    }
    assert!(window.is_open() && window.pair(None, Some(&s(&new_code))));
}

#[test]
fn test_close_ends_a_window_and_pairing_secrets_never_reach_a_log() {
    let window = PairingWindow::with_clock(FakeClock::new().clock());
    let (values, log) = caplog(|| {
        let (secret, code) = window.open();
        window.pair(Some(&s(CANARY)), None);
        window.close();
        assert!(!window.is_open() && !window.pair(Some(&s(&secret)), None));
        let (secret2, code2) = window.open();
        window.pair(None, Some(&s(&code2)));
        [secret, code, secret2, code2, CANARY.to_string()]
    });
    assert!(log.contains("pairing window open") && log.contains("paired through the window"), "{log}");
    for value in values {
        assert!(!log.contains(&value));
    }
}

// ---- the Mac's own login --------------------------------------------------------------------------------

#[test]
fn test_a_mac_login_code_works_once() {
    let login = MacLogin::with_clock(FakeClock::new().clock());
    let code = login.issue();
    assert!(re("[A-Za-z0-9_-]{22}").is_match(&code)); // 128 bits
    assert!(!login.used(Some(&code)));
    assert!(login.consume(Some(&code)));
    assert!(login.used(Some(&code)));
    assert!(!login.consume(Some(&code)));
}

#[test]
fn test_a_mac_login_code_expires() {
    let clock = FakeClock::new();
    let login = MacLogin::with_clock(clock.clock());
    let code = login.issue_for(120.0);
    clock.advance(120.0);
    assert!(!login.consume(Some(&code)) && !login.used(Some(&code)));
    let used = login.issue_for(120.0);
    assert!(login.consume(Some(&used)));
    clock.advance(120.0 + LOGIN_KEEP_SECS - 1.0);
    assert!(login.used(Some(&used))); // `web open` may still ask, a little after
    clock.advance(1.0);
    assert!(!login.used(Some(&used)));
}

#[test]
fn test_mac_login_codes_are_independent_and_bounded() {
    let login = MacLogin::with_clock(FakeClock::new().clock());
    let (first, second) = (login.issue(), login.issue());
    assert_ne!(first, second);
    assert!(login.consume(Some(&second)) && !login.used(Some(&first)) && login.consume(Some(&first)));
    let many: Vec<String> = (0..=MAX_LOGINS).map(|_| login.issue()).collect();
    assert!(!login.consume(Some(&many[0]))); // the oldest made room
    assert!(many[1..].iter().all(|c| login.consume(Some(c))));
}

#[test]
fn test_mac_login_refuses_what_it_never_issued() {
    let login = MacLogin::with_clock(FakeClock::new().clock());
    login.issue();
    let long = "x".repeat(1000);
    // Python also refuses None and 123456: None stands for any non-string here
    for wrong in [Some(CANARY), Some(""), None, Some("123456"), Some(long.as_str()), Some("bad code with spaces")] {
        assert!(!login.consume(wrong) && !login.used(wrong), "{wrong:?}");
    }
}

#[test]
fn test_mac_login_codes_are_held_only_as_hashes() {
    let login = MacLogin::with_clock(FakeClock::new().clock());
    let (code, log) = caplog(|| {
        let code = login.issue();
        login.consume(Some(&code));
        code
    });
    assert!(log.contains("signed in") && !log.contains(&code), "{log}");
    let held = format!("{login:?}");
    assert!(!held.contains(&code) && held.contains(&sha_hex(&code)), "{held}"); // control: the hash is
}
