//! Port of bridge/tests/test_vault.py (the Keychain wrapper, the index, site binding, 2FA codes and
//! `eve-bridge vault`), the vault-side assertions of test_vault_fill.py, and parity checks against bytes the
//! Python wrote.
//!
//! Everything but the one live test runs on a fake `security`; the live test uses a throwaway keychain file
//! and never the owner's login keychain.

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::{json, Value};

use super::*;

fn values(v: Value) -> Values {
    match v {
        Value::Object(m) => m,
        _ => panic!("not an object"),
    }
}

fn fill(pairs: &[(&str, &str)]) -> FillValues {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// `shlex.split` for the words `security -i` is sent (no quoting is ever needed: the value is hex).
fn words(line: &str) -> Vec<String> {
    line.split_whitespace().map(str::to_string).collect()
}

/// /usr/bin/security over a map: records every argv and stdin so a test can look for a leaked value.
#[derive(Default)]
struct FakeSecurity {
    items: Mutex<IndexMap<String, String>>,
    calls: Mutex<Vec<(Vec<String>, String)>>,
}

impl Runner for Arc<FakeSecurity> {
    fn run(&self, argv: &[String], input: Option<&str>, _timeout: Duration) -> io::Result<RunOutput> {
        self.calls.lock().push((argv.to_vec(), input.unwrap_or("").to_string()));
        let ok = |stdout: String| Ok(RunOutput { returncode: 0, stdout, stderr: String::new() });
        if argv[1..] == ["-i".to_string()] {
            let w = words(input.unwrap_or(""));
            assert_eq!(w[0], "add-generic-password");
            let at = |flag: &str| w[w.iter().position(|x| x == flag).expect("flag") + 1].clone();
            self.items.lock().insert(at("-a"), at("-w"));
            return ok(String::new());
        }
        let handle = argv[argv.iter().position(|x| x == "-a").expect("-a") + 1].clone();
        match argv[1].as_str() {
            "find-generic-password" => match self.items.lock().get(&handle) {
                Some(v) => ok(format!("{v}\n")),
                None => Ok(RunOutput { returncode: 44, stdout: String::new(), stderr: "not found".into() }),
            },
            "delete-generic-password" => {
                let gone = self.items.lock().shift_remove(&handle).is_some();
                Ok(RunOutput { returncode: if gone { 0 } else { 44 }, ..Default::default() })
            }
            other => panic!("unexpected security call {other}"),
        }
    }
}

fn make_vault(dir: &Path, at: f64) -> (Vault, Arc<FakeSecurity>) {
    let fake = Arc::new(FakeSecurity::default());
    let keychain = Keychain::with_env("", None, Box::new(fake.clone())).expect("keychain");
    (Vault::with_clock(dir.join("vault.json"), Box::new(keychain), move || at), fake)
}

fn b32(seed: &[u8]) -> String {
    // RFC 4648 base32, as Python's base64.b32encode
    const A: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::new();
    for chunk in seed.chunks(5) {
        let mut buf = [0u8; 5];
        buf[..chunk.len()].copy_from_slice(chunk);
        let n = u64::from_be_bytes([0, 0, 0, buf[0], buf[1], buf[2], buf[3], buf[4]]);
        let chars = match chunk.len() {
            1 => 2,
            2 => 4,
            3 => 5,
            4 => 7,
            _ => 8,
        };
        for i in 0..8 {
            if i < chars {
                out.push(A[((n >> (35 - 5 * i)) & 31) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

// ---- 2FA codes ------------------------------------------------------------------------------------------------

/// RFC 6238 appendix B: time, SHA1, SHA256, SHA512 (8 digits).
const RFC6238: [(f64, &str, &str, &str); 6] = [
    (59.0, "94287082", "46119246", "90693936"),
    (1111111109.0, "07081804", "68084774", "25091201"),
    (1111111111.0, "14050471", "67062674", "99943326"),
    (1234567890.0, "89005924", "91819424", "93441116"),
    (2000000000.0, "69279037", "90698825", "38618901"),
    (20000000000.0, "65353130", "77737706", "47863826"),
];

fn seed(algo: &str) -> Vec<u8> {
    match algo {
        "SHA1" => b"12345678901234567890".to_vec(),
        "SHA256" => b"12345678901234567890123456789012".to_vec(),
        _ => [b"1234567890".repeat(6), b"1234".to_vec()].concat(),
    }
}

#[test]
fn test_totp_matches_the_rfc_6238_vectors() {
    for (at, a, b, c) in RFC6238 {
        for (algo, want) in [("SHA1", a), ("SHA256", b), ("SHA512", c)] {
            let secret = b32(&seed(algo));
            assert_eq!(totp(&secret, Some(at), 8, 30, algo).unwrap(), want, "{at} {algo}");
        }
    }
}

#[test]
fn test_a_2fa_secret_is_read_from_a_uri_or_bare() {
    let secret = b32(&seed("SHA1")).trim_end_matches('=').to_string();
    let got = parse_totp(&format!(
        "otpauth://totp/Example:me?secret={secret}&issuer=Example&digits=8&period=30&algorithm=SHA256"
    ))
    .unwrap();
    assert_eq!(got, values(json!({"secret": secret, "digits": 8, "period": 30, "algorithm": "SHA256"})));
    let lower = secret.to_lowercase();
    let spaced = format!(" {}-{} ", &lower[..8], &lower[8..]);
    assert_eq!(parse_totp(&spaced).unwrap()["secret"], json!(secret));
    for bad in [
        String::new(),
        "not base32!".into(),
        format!("otpauth://hotp/x?secret={secret}"),
        format!("otpauth://totp/x?secret={secret}&digits=4"),
    ] {
        assert!(parse_totp(&bad).is_err(), "{bad}");
    }
}

#[test]
fn parse_totp_keeps_pythons_key_order_and_lowercased_query_names() {
    let got = parse_totp("otpauth://totp/x?secret=gezd%20gnbv&DIGITS=7&Period=60&algorithm=sha512").unwrap();
    let keys: Vec<&str> = got.keys().map(String::as_str).collect();
    assert_eq!(keys, ["digits", "period", "algorithm", "secret"]);
    assert_eq!(got, values(json!({"digits": 7, "period": 60, "algorithm": "SHA512", "secret": "GEZDGNBV"})));
    assert!(parse_totp("otpauth://totp/x?secret=GEZDGNBV&digits=x").unwrap_err().message().contains("not a number"));
}

#[test]
fn b32_key_decodes_like_python() {
    assert_eq!(hex::encode(b32_key("AB").unwrap()), "00");
    assert_eq!(hex::encode(b32_key("GEZDGNBVGY3TQOJQ").unwrap()), "31323334353637383930");
    assert_eq!(hex::encode(b32_key("GEZDGNBVGY3TQOJQGE").unwrap()), "3132333435363738393031");
    assert!(b32_key("A").is_err()); // Python: binascii.Error, Incorrect padding
}

// ---- sites ----------------------------------------------------------------------------------------------------

#[test]
fn test_a_site_is_the_saved_host_or_its_subdomains_on_the_same_scheme_and_port() {
    assert_eq!(canonical_origin("chase.com").unwrap(), "https://chase.com");
    assert_eq!(canonical_origin("HTTPS://Secure.Chase.com/login?x=1").unwrap(), "https://secure.chase.com");
    assert_eq!(canonical_origin("http://127.0.0.1:8123/a").unwrap(), "http://127.0.0.1:8123");
    assert_eq!(canonical_origin("https://chase.com:443").unwrap(), "https://chase.com");
    for bad in ["", "ftp://chase.com", "https://user:pw@chase.com", "chase com"] {
        assert!(canonical_origin(bad).is_err(), "{bad}");
    }
    let yes = ["https://chase.com/", "https://secure.chase.com/login", "https://a.b.chase.com", "https://chase.com:443/x"];
    let no = [
        "https://chase.com.evil.example/",
        "https://evilchase.com/",
        "http://chase.com/",
        "https://chase.com:8443/",
        "https://com/",
        "https://user@chase.com/",
        "",
        "not a url",
        "javascript:alert(1)",
    ];
    for url in yes {
        assert!(site_allows("chase.com", url), "{url}");
    }
    for url in no {
        assert!(!site_allows("chase.com", url), "{url}");
    }
    assert!(!site_allows("secure.chase.com", "https://chase.com/")); // never the parent
    assert!(site_allows("http://127.0.0.1:8123", "http://127.0.0.1:8123/login.html"));
    assert!(!site_allows("http://127.0.0.1:8123", "http://127.0.0.1:9999/login.html"));
}

#[test]
fn sites_match_python_on_odd_inputs() {
    assert_eq!(canonical_origin("https://Chase.com.:0").unwrap(), "https://chase.com");
    assert!(site_allows("chase.com", "https://chase.com:0/"));
    assert_eq!(canonical_origin("https://[::1]:8443").unwrap(), "https://::1:8443");
    assert_eq!(site_of("https://[::1]:8443"), "::1:8443");
    assert_eq!(canonical_origin("x y").unwrap_err().message(), "not a site: 'x y'");
    assert_eq!(canonical_origin("  ").unwrap_err().message(), "a login or 2FA item needs the site it is for");
    assert!(canonical_origin("https://chase.com:99999").is_err());
    assert!(!site_allows("chase.com", "https://chase.com:x/"));
}

// ---- the Keychain wrapper and the index ----------------------------------------------------------------------

#[test]
fn test_a_secret_never_rides_on_a_command_line_and_the_index_holds_none() {
    let tmp = tempfile::tempdir().unwrap();
    let (v, fake) = make_vault(tmp.path(), 59.0);
    let login = v
        .add("login", "Chase", &values(json!({"username": "me@example.com", "password": "hunter2 with spaces \"q\""})),
             "https://chase.com/login")
        .unwrap();
    let card = v
        .add("card", "Visa", &values(json!({"number": "4242 4242 4242 4242", "exp_month": "7", "exp_year": "29",
                                             "cvc": "123", "name": "A Person"})), "")
        .unwrap();
    let code = v.add("totp", "Chase 2FA", &values(json!({"secret": b32(&seed("SHA1"))})), "chase.com").unwrap();
    for (argv, _stdin) in fake.calls.lock().iter() {
        let joined = argv.join(" ");
        for s in ["hunter2", "4242424242424242", "123 ", &b32(&seed("SHA1"))] {
            assert!(!joined.contains(s), "{s} in {argv:?}");
        }
    }
    let index_path = tmp.path().join("vault.json");
    let index = std::fs::read_to_string(&index_path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&index_path).unwrap().permissions().mode() & 0o777, 0o600);
    }
    for s in ["hunter2", "4242", "me@example.com", "GEZDGNBV"] {
        assert!(!index.contains(s), "{s}");
    }
    let lines: Vec<String> = v.items().iter().map(Item::line).collect();
    assert_eq!(
        lines,
        [
            format!("{} login \"Chase\" for chase.com", login.handle),
            format!("{} card \"Visa\"", card.handle),
            format!("{} totp \"Chase 2FA\" for chase.com", code.handle),
        ]
    );
    let items = v.items();
    assert!(items.iter().all(|i| HANDLE.is_match(&i.handle)));
    let unique: std::collections::HashSet<_> = items.iter().map(|i| i.handle.clone()).collect();
    assert_eq!(unique.len(), 3);
    let (_, got) = v.fill_values(&login.handle, "https://secure.chase.com/x", "").unwrap();
    assert_eq!(got, fill(&[("username", "me@example.com"), ("password", "hunter2 with spaces \"q\"")]));
}

#[test]
fn test_a_fill_is_refused_off_its_site_and_a_card_fills_anywhere() {
    let tmp = tempfile::tempdir().unwrap();
    let (v, _) = make_vault(tmp.path(), 59.0);
    let login = v.add("login", "Chase", &values(json!({"username": "", "password": "pw"})), "chase.com").unwrap();
    let card = v
        .add("card", "Visa", &values(json!({"number": "4242424242424242", "exp_month": "07", "exp_year": "2029", "cvc": "1234"})), "")
        .unwrap();
    let code = v.add("totp", "2FA", &values(json!({"secret": b32(&seed("SHA1"))})), "chase.com").unwrap();
    let e = v.fill_values(&login.handle, "https://chase.com.evil.example/login", "").unwrap_err();
    assert!(e.is_refused() && e.message().contains("saved for chase.com"), "{e}");
    assert!(v.fill_values(&code.handle, "https://example.com/", "").unwrap_err().is_refused());
    let e = v.fill_values("v_00000000", "https://chase.com/", "").unwrap_err();
    assert!(e.is_refused() && e.message().contains("no vault item"), "{e}");
    let e = v.fill_values(&login.handle, "https://chase.com/", "cvc").unwrap_err();
    assert!(e.is_refused() && e.message().contains("has no"), "{e}");
    assert_eq!(e.message(), "a login has no 'cvc'; its parts are username, password");
    // no username saved
    assert_eq!(v.fill_values(&login.handle, "https://chase.com/", "").unwrap().1, fill(&[("password", "pw")]));
    let (_, c) = v.fill_values(&card.handle, "https://shop.example/checkout", "").unwrap();
    assert_eq!(
        c,
        fill(&[("number", "4242424242424242"), ("exp_month", "07"), ("exp_year", "2029"), ("exp", "07/29"), ("cvc", "1234")])
    );
    assert_eq!(v.fill_values(&card.handle, "https://x.example/", "cvc").unwrap().1, fill(&[("cvc", "1234")]));
    // RFC vector, 6 digits
    assert_eq!(v.fill_values(&code.handle, "https://login.chase.com/", "").unwrap().1, fill(&[("code", "287082")]));
}

#[test]
fn test_bad_items_are_refused_before_the_keychain_is_touched() {
    let tmp = tempfile::tempdir().unwrap();
    let (v, fake) = make_vault(tmp.path(), 59.0);
    let cases = [
        ("login", json!({"password": ""}), "chase.com"),
        ("login", json!({"password": "x"}), ""),
        ("card", json!({"number": "42", "exp_month": "1", "exp_year": "29", "cvc": "123"}), ""),
        ("card", json!({"number": "4242424242424242", "exp_month": "13", "exp_year": "29", "cvc": "123"}), ""),
        ("card", json!({"number": "4242424242424242", "exp_month": "1", "exp_year": "29", "cvc": "12"}), ""),
        ("totp", json!({"secret": "nope!"}), "chase.com"),
        ("pin", json!({}), ""),
    ];
    for (kind, value, site) in cases {
        assert!(v.add(kind, "x", &values(value), site).is_err(), "{kind}");
    }
    assert!(fake.calls.lock().is_empty() && v.items().is_empty());
    // review A11 (2026-10-18): a blank label raised only after the secret was in the Keychain, unindexed
    for label in ["", "   "] {
        assert!(v.add("login", label, &values(json!({"username": "a", "password": "hunter2"})), "chase.com").is_err());
    }
    assert!(fake.calls.lock().is_empty() && fake.items.lock().is_empty() && v.items().is_empty());
}

#[test]
fn test_remove_deletes_the_keychain_item_and_the_index_line() {
    let tmp = tempfile::tempdir().unwrap();
    let (v, fake) = make_vault(tmp.path(), 59.0);
    let a = v.add("login", "A", &values(json!({"password": "a"})), "a.example").unwrap();
    let b = v.add("login", "B", &values(json!({"password": "b"})), "b.example").unwrap();
    assert!(v.remove(&a.handle).unwrap() && !v.remove(&a.handle).unwrap());
    assert_eq!(v.items().iter().map(|i| i.handle.as_str()).collect::<Vec<_>>(), [b.handle.as_str()]);
    assert_eq!(fake.items.lock().keys().cloned().collect::<Vec<_>>(), [b.handle]);
}

// ---- parity with the bytes Python writes ---------------------------------------------------------------------

#[test]
fn the_index_is_written_byte_for_byte_as_python_writes_it() {
    let tmp = tempfile::tempdir().unwrap();
    let v = Vault::with_store(tmp.path().join("sub").join("vault.json"), Box::new(MemKeychain::new()));
    v.write(&[Item::new("v_abcdefgh", "login", "Café 😀", "https://chase.com")]).unwrap();
    // json.dumps({...}, indent=1) from Python 3.13
    assert_eq!(
        std::fs::read_to_string(v.index_path()).unwrap(),
        "{\n \"items\": [\n  {\n   \"handle\": \"v_abcdefgh\",\n   \"kind\": \"login\",\n   \"label\": \"Caf\\u00e9 \\ud83d\\ude00\",\n   \"origin\": \"https://chase.com\"\n  }\n ]\n}"
    );
    assert_eq!(v.items()[0].label, "Café 😀");
    v.write(&[]).unwrap();
    assert_eq!(std::fs::read_to_string(v.index_path()).unwrap(), "{\n \"items\": []\n}");
    assert!(!tmp.path().join("sub").join("vault.json.tmp").exists());
}

#[test]
fn the_keychain_blob_is_the_hex_python_writes() {
    let fake = Arc::new(FakeSecurity::default());
    let k = Keychain::with_env("", None, Box::new(fake.clone())).unwrap();
    k.put("v_abcdefgh", &values(json!({"username": "me", "password": "pé \"q\"\\"}))).unwrap();
    let (argv, stdin) = fake.calls.lock()[0].clone();
    assert_eq!(argv, ["/usr/bin/security", "-i"]);
    assert_eq!(
        stdin,
        "add-generic-password -U -s eve-vault -a v_abcdefgh -l eve-vault -w \
         7b22757365726e616d65223a226d65222c2270617373776f7264223a22705c7530306539205c22715c225c5c227d\n"
    );
    assert!(k.put("bad", &Values::new()).is_err());
}

#[test]
fn the_keychain_names_another_keychain_file_and_refuses_a_quoted_one() {
    let fake = Arc::new(FakeSecurity::default());
    let k = Keychain::with_env("", Some(" /tmp/x.keychain-db "), Box::new(fake.clone())).unwrap();
    assert_eq!(k.keychain, "/tmp/x.keychain-db");
    let _ = k.get("v_abcdefgh").unwrap();
    assert_eq!(fake.calls.lock()[0].0.last().unwrap(), "/tmp/x.keychain-db");
    for bad in ["/tmp/a b", "/tmp/\"x", "/tmp/'x", "/tmp/\\x"] {
        assert!(Keychain::with_env(bad, None, Box::new(fake.clone())).is_err(), "{bad}");
    }
    assert!(Keychain::with_env("", Some("/tmp/a b"), Box::new(fake)).is_err());
}

#[test]
fn the_index_reader_skips_what_python_skips() {
    let tmp = tempfile::tempdir().unwrap();
    let v = Vault::with_store(tmp.path().join("vault.json"), Box::new(MemKeychain::new()));
    assert!(v.items().is_empty()); // no file
    std::fs::write(v.index_path(), "not json").unwrap();
    assert!(v.items().is_empty());
    std::fs::write(
        v.index_path(),
        r#"{"items": [
            {"handle": "v_abcdefgh", "kind": "card", "label": 5},
            {"handle": "v_abcdefgh\n", "kind": "login", "label": "nl", "origin": "https://a.example"},
            {"handle": "v_ABCDEFGH", "kind": "card", "label": "upper"},
            {"handle": "v_abcdefgi", "kind": "pin", "label": "pin"},
            {"handle": "v_abcdefgj", "kind": "card"},
            {"handle": 7, "kind": "card", "label": "x"},
            "junk",
            {"handle": "v_abcdefgk", "kind": "totp", "label": null, "origin": 0}
        ]}"#,
    )
    .unwrap();
    let items = v.items();
    assert_eq!(
        items,
        [
            Item::new("v_abcdefgh", "card", "5", ""),
            Item::new("v_abcdefgh\n", "login", "nl", "https://a.example"),
            Item::new("v_abcdefgk", "totp", "None", ""),
        ]
    );
    assert_eq!(v.lines(1), ["v_abcdefgh card \"5\""]);
    assert_eq!(v.get(" v_abcdefgk ").map(|i| i.label), Some("None".to_string()));
}

#[test]
fn labels_are_cleaned_as_python_cleans_them() {
    assert_eq!(clean_label("  Visa \"gold\"\n card ").unwrap(), "Visa 'gold' card");
    assert_eq!(clean_label(&"x".repeat(70)).unwrap().len(), MAX_LABEL_CHARS);
    assert_eq!(clean_label("\t").unwrap_err().message(), "an item needs a label");
}

#[test]
fn a_card_is_normalized_as_python_normalizes_it() {
    let c = card(&values(json!({"number": "4242-4242 4242-4242", "exp_month": 7, "exp_year": 29, "cvc": " 123 ",
                                "name": "  A   Person "}))).unwrap();
    assert_eq!(
        c,
        values(json!({"number": "4242424242424242", "exp_month": "07", "exp_year": "2029", "cvc": "123", "name": "A Person"}))
    );
    assert_eq!(card(&values(json!({"number": "4242424242424242", "exp_month": "1", "exp_year": "202", "cvc": "123"})))
        .unwrap_err().message(), "a card needs its expiry month (1-12) and year");
}

#[test]
fn the_default_index_follows_eve_vault_index() {
    assert!(default_index_from(None).ends_with(".config/eve-bridge/vault.json"));
    assert!(default_index_from(Some("  ")).ends_with(".config/eve-bridge/vault.json"));
    assert_eq!(default_index_from(Some("/tmp/x/vault.json")), PathBuf::from("/tmp/x/vault.json"));
    assert_eq!(default_index_from(Some("~/v.json")), home().join("v.json"));
}

// ---- test_vault_fill.py: the vault side of the page's refusal --------------------------------------------------

#[test]
fn test_the_page_refuses_before_touching_the_page_off_site_or_without_a_vault() {
    let tmp = tempfile::tempdir().unwrap();
    let v = Vault::with_store(tmp.path().join("vault.json"), Box::new(MemKeychain::new()));
    let item = v.add("login", "Chase", &values(json!({"username": "me", "password": "pw"})), "chase.com").unwrap();
    let e = v.fill_values(&item.handle, "https://chase.com.evil.example/login", "").unwrap_err();
    assert!(e.is_refused() && e.message().contains("saved for chase.com"));
    let e = v.fill_values("v_zzzzzzzz", "https://chase.com/", "").unwrap_err();
    assert!(e.message().starts_with("no vault item"));
    assert_eq!(e.message(), "no vault item 'v_zzzzzzzz'");
}

#[test]
fn a_live_fill_reads_each_kind_from_a_mem_keychain() {
    // test_vault_fill.py's live test, minus the browser: the values its pages are filled with.
    let tmp = tempfile::tempdir().unwrap();
    let v = Vault::with_clock(tmp.path().join("vault.json"), Box::new(MemKeychain::new()), || 1_234_567_890.0);
    let base = "http://127.0.0.1:8123";
    let secret = b32(b"12345678901234567890");
    let card = v
        .add("card", "Visa", &values(json!({"number": "4000056655665556", "exp_month": "7", "exp_year": "2031",
                                             "cvc": "713", "name": "Canary Holder"})), "")
        .unwrap();
    let code = v.add("totp", "Local 2FA", &values(json!({"secret": secret})), base).unwrap();
    assert_eq!(code.line(), format!("{} totp \"Local 2FA\" for 127.0.0.1:8123", code.handle));
    let (_, c) = v.fill_values(&card.handle, &format!("{base}/card.html"), "").unwrap();
    assert_eq!(c["exp"], "07/31");
    assert_eq!(c["name"], "Canary Holder");
    let want = totp(&secret, Some(1_234_567_890.0), 6, 30, "SHA1").unwrap();
    assert_eq!(v.fill_values(&code.handle, &format!("{base}/otp.html"), "code").unwrap().1["code"], want);
}

// ---- eve-bridge vault -----------------------------------------------------------------------------------------

#[test]
fn test_the_cli_reads_secrets_with_getpass_and_prints_no_value() {
    let tmp = tempfile::tempdir().unwrap();
    let (v, _) = make_vault(tmp.path(), 59.0);
    let secret_prompts: Mutex<Vec<String>> = Mutex::new(vec![]);
    let totp_secret = b32(&seed("SHA1"));
    let mut ask_secret = |prompt: &str| -> io::Result<String> {
        secret_prompts.lock().push(prompt.to_string());
        Ok(match prompt {
            "Password: " => "pw-canary".into(),
            "Card number: " => "4242424242424242".into(),
            "CVC: " => "321".into(),
            "2FA secret or otpauth:// URI: " => totp_secret.clone(),
            other => panic!("unexpected secret prompt {other}"),
        })
    };
    let mut ask = |prompt: &str| -> io::Result<String> {
        Ok(match prompt {
            "Username: " => "me".into(),
            "Expiry (MM/YY): " => "08/30".into(),
            "Name on card: " => "A Person".into(),
            other => panic!("unexpected prompt {other}"),
        })
    };
    let mut out: Vec<u8> = vec![];
    let mut run = |args: &[&str]| -> i32 {
        let argv: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        cli(&argv, Some(&v), &mut ask, &mut ask_secret, &mut out)
    };
    assert_eq!(run(&["add", "login", "--label", "Chase", "--site", "chase.com"]), 0);
    assert_eq!(run(&["add", "card", "--label", "Visa 4242"]), 0);
    assert_eq!(run(&["add", "totp", "--label", "Chase 2FA", "--site", "chase.com"]), 0);
    assert_eq!(run(&["list"]), 0);
    let handle = v.items()[0].handle.clone();
    assert_eq!(run(&["remove", &handle]), 0);
    assert_eq!(run(&["remove", &handle]), 1);
    assert_eq!(run(&["add", "login", "--label", "No site"]), 2); // Python: SystemExit
    assert_eq!(*secret_prompts.lock(), ["Password: ", "Card number: ", "CVC: ", "2FA secret or otpauth:// URI: "]);
    let text = String::from_utf8(out).unwrap();
    for s in ["pw-canary", "4242424242424242", "321", "GEZDGNBV"] {
        assert!(!text.contains(s), "{s}");
    }
    assert!(text.contains("card \"Visa 4242\"") && text.contains("for chase.com"));
    assert!(text.contains("Removed.") && text.contains(&format!("No item '{handle}'.")));
    let (_, card) = v.fill_values(&v.items()[0].handle, "https://x.example/", "").unwrap();
    assert_eq!(card["exp"], "08/30");
}

#[test]
fn the_cli_parses_arguments_as_argparse_does() {
    let tmp = tempfile::tempdir().unwrap();
    let (v, _) = make_vault(tmp.path(), 59.0);
    let mut ask = |_: &str| -> io::Result<String> { Ok("me".into()) };
    let mut ask_secret = |_: &str| -> io::Result<String> { Ok("pw".into()) };
    let mut out: Vec<u8> = vec![];
    let mut run = |args: &[&str]| -> i32 {
        let argv: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        cli(&argv, Some(&v), &mut ask, &mut ask_secret, &mut out)
    };
    // usage errors exit 2, as argparse's SystemExit(2)
    for bad in [
        &[][..],
        &["bogus"],
        &["add"],
        &["add", "pin", "--label", "x"],
        &["add", "login"],
        &["add", "login", "--label"],
        &["add", "login", "--label", "-x", "--site", "a.example"],
        &["add", "login", "--label=X", "--site"],
        &["list", "extra"],
        &["remove"],
        &["remove", "-x"],
        &["remove", "a", "b"],
        &["add", "login", "--label", "X", "--s="],
        &["-h=x"],
        &["--help="],
    ] {
        assert_eq!(run(bad), 2, "{bad:?}");
    }
    // help exits 0
    for help in [&["--help"][..], &["--h"], &["-h", "list"], &["add", "--help"], &["remove", "-h", "x"], &["-hx"]] {
        assert_eq!(run(help), 0, "{help:?}");
    }
    assert!(v.items().is_empty());
    // abbreviations, --opt=value, options before the kind, the last of a repeated option
    assert_eq!(run(&["add", "--lab", "First", "--label=Second", "login", "--si=a.example"]), 0);
    assert_eq!(v.items()[0].line(), format!("{} login \"Second\" for a.example", v.items()[0].handle));
    // `--` makes what follows positional
    assert_eq!(run(&["remove", "--", "-x"]), 1);
    // a negative number is a value, not an option
    assert_eq!(run(&["add", "card", "--label", "-5"]), 1); // reaches the vault: "me" is no card number
}

// ---- live: a throwaway keychain file ---------------------------------------------------------------------------

#[test]
#[ignore = "live: creates a throwaway keychain with /usr/bin/security"]
fn test_live_keychain_round_trip_in_a_throwaway_keychain() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("eve-vault-test.keychain-db").to_string_lossy().into_owned();
    let password = "eve-test-keychain";
    let sec = |args: &[&str]| std::process::Command::new(SECURITY).args(args).output().unwrap();
    assert!(sec(&["create-keychain", "-p", password, &path]).status.success());
    let result = std::panic::catch_unwind(|| {
        assert!(sec(&["unlock-keychain", "-p", password, &path]).status.success());
        let keychain = Keychain::with_env(&path, None, Box::new(SubprocessRunner)).unwrap();
        let v = Vault::with_store(tmp.path().join("vault.json"), Box::new(keychain));
        let secret = b32(&seed("SHA1"));
        let login = v
            .add("login", "Local", &values(json!({"username": "me", "password": "p@ss w0rd \"'\\ canary"})),
                 "http://127.0.0.1:8000")
            .unwrap();
        let card = v
            .add("card", "Visa", &values(json!({"number": "4242424242424242", "exp_month": "7", "exp_year": "2031",
                                                 "cvc": "987", "name": "A Person"})), "")
            .unwrap();
        let code = v.add("totp", "2FA", &values(json!({"secret": secret})), "http://127.0.0.1:8000").unwrap();
        assert_eq!(
            v.keychain.get(&login.handle).unwrap().unwrap(),
            values(json!({"username": "me", "password": "p@ss w0rd \"'\\ canary"}))
        );
        assert_eq!(v.keychain.get(&card.handle).unwrap().unwrap()["number"], json!("4242424242424242"));
        assert_eq!(v.keychain.get(&code.handle).unwrap().unwrap()["secret"], json!(secret));
        assert_eq!(
            v.fill_values(&code.handle, "http://127.0.0.1:8000/", "").unwrap().1["code"],
            totp(&secret, None, 6, 30, "SHA1").unwrap()
        );
        // nothing landed in the owner's keychains: the default search list does not have these handles
        for item in [&login, &card, &code] {
            assert!(!sec(&["find-generic-password", "-s", SERVICE, "-a", &item.handle]).status.success());
        }
        for item in [&login, &card, &code] {
            assert!(v.remove(&item.handle).unwrap());
        }
        let dump = String::from_utf8_lossy(&sec(&["dump-keychain", &path]).stdout).into_owned();
        assert!(!dump.contains(SERVICE) && v.items().is_empty());
    });
    let _ = sec(&["delete-keychain", &path]);
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
    let index: Value = serde_json::from_str(&std::fs::read_to_string(tmp.path().join("vault.json")).unwrap()).unwrap();
    assert_eq!(index, json!({"items": []}));
}
