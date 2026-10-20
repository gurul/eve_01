//! Port of bridge/tests/test_vault_mcp.py: the vault's two tools for Codex tasks and which field a value may go
//! into. The Codex wiring tests there (`_thread_params`, `_vault_changed`, `vault_tool_call`) test
//! codex_computer and belong to its crate; `server_config` is tested here.

use std::collections::HashSet;
use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;

use eve_vault::{totp_default, MemKeychain, Values, Vault};
use eve_vault_native::{Field, Native};
use parking_lot::Mutex;
use serde_json::{json, Value};

use super::*;

/// Pages and their fields; page_fields walks only the pages `want` accepts, as the real one does.
#[derive(Default)]
struct FakeMac {
    fields: Vec<Field>,
    takes: bool,
    count: Option<i64>,
    /// (the field's words, the value)
    set: Mutex<Vec<(String, String)>>,
    walked: Mutex<HashSet<String>>,
}

impl FakeMac {
    fn new(fields: Vec<Field>) -> Self {
        FakeMac { fields, takes: true, ..Default::default() }
    }

    fn set(&self) -> Vec<(String, String)> {
        self.set.lock().clone()
    }
}

impl Native for FakeMac {
    fn trusted(&self) -> bool {
        true
    }

    fn page_fields(&self, want: &dyn Fn(&str) -> bool, frames: bool) -> Vec<Field> {
        let out: Vec<Field> =
            self.fields.iter().filter(|f| want(&f.url) || (frames && !f.page.is_empty() && want(&f.page))).cloned().collect();
        self.walked.lock().extend(out.iter().map(|f| f.url.clone()));
        out
    }

    fn set_value(&self, field: &Field, text: &str) -> bool {
        if self.takes {
            self.set.lock().push((field.words.clone(), text.to_string()));
        }
        self.takes
    }

    fn char_count(&self, _field: &Field) -> Option<i64> {
        Some(self.count.unwrap_or_else(|| self.set.lock().last().map_or(0, |(_, v)| v.chars().count() as i64)))
    }
}

/// A writer the test can read back.
#[derive(Clone, Default)]
struct Buf(Arc<Mutex<Vec<u8>>>);

impl Buf {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().clone()).unwrap()
    }
}

impl Write for Buf {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.lock().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

const CANARY_PASSWORD: &str = "mcp-canary-pw!";
const CANARY_NUMBER: &str = "4000056655665556";
const CANARY_CVC: &str = "713";
const CANARY: [&str; 3] = [CANARY_PASSWORD, CANARY_NUMBER, CANARY_CVC];
/// `base64.b32encode(b"12345678901234567890").decode()`
const SECRET: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";
const LOGIN: &str = "https://secure.chase.com/login";

fn values(v: Value) -> Values {
    v.as_object().unwrap().clone()
}

struct Ids {
    login: String,
    card: String,
    totp: String,
}

fn setup(tmp: &Path) -> (Arc<Vault>, Ids) {
    let v = Vault::with_clock(tmp.join("vault.json"), Box::new(MemKeychain::new()), || 59.0);
    let login = v
        .add("login", "Chase", &values(json!({"username": "me@example.com", "password": CANARY_PASSWORD})), "chase.com")
        .unwrap()
        .handle;
    let card = v
        .add(
            "card",
            "Visa",
            &values(json!({"number": CANARY_NUMBER, "exp_month": "7", "exp_year": "2031", "cvc": CANARY_CVC})),
            "",
        )
        .unwrap()
        .handle;
    let totp = v.add("totp", "Chase 2FA", &values(json!({"secret": SECRET})), "chase.com").unwrap().handle;
    (Arc::new(v), Ids { login, card, totp })
}

fn f(words: &str, url: &str, secure: bool, order: usize, focused: bool) -> Field {
    Field {
        pid: 1,
        app: "Google Chrome".into(),
        role: "AXTextField".into(),
        subrole: if secure { "AXSecureTextField".into() } else { String::new() },
        url: url.into(),
        words: words.into(),
        focused,
        order,
        ..Default::default()
    }
}

/// `f(words)` with the Python helper's defaults.
fn fw(words: &str) -> Field {
    f(words, LOGIN, false, 1, false)
}

fn code() -> String {
    totp_default(SECRET, Some(59.0)).unwrap()
}

#[test]
fn test_the_server_speaks_mcp_and_lists_no_value() {
    let tmp = tempfile::tempdir().unwrap();
    let (v, ids) = setup(tmp.path());
    let out = Buf::default();
    let server = Server::new(v, Some(Arc::new(FakeMac::new(vec![]))), Box::new(out.clone()));
    for msg in [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "vault_list", "arguments": {}}}),
        json!({"jsonrpc": "2.0", "id": 4, "method": "nope"}),
    ] {
        server.handle_line(&serde_json::to_string(&msg).unwrap()).unwrap();
    }
    server.handle_line("not json").unwrap();
    let text = out.text();
    let replies: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let got_ids: Vec<Value> = replies.iter().map(|r| r.get("id").cloned().unwrap_or(Value::Null)).collect();
    assert_eq!(got_ids, vec![json!(1), json!(2), json!(3), json!(4), Value::Null]);
    assert_eq!(replies[0]["result"]["serverInfo"]["name"], "eve_vault");
    assert_eq!(replies[0]["result"]["protocolVersion"], "2025-06-18");
    let names: Vec<&str> = replies[1]["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["vault_list", "vault_fill"]);
    let listing = replies[2]["result"]["content"][0]["text"].as_str().unwrap();
    assert!(listing.contains(&format!("{} login \"Chase\" for chase.com", ids.login)), "{listing}");
    assert!(listing.contains(&format!("{} card \"Visa\"", ids.card)), "{listing}");
    assert_eq!(replies[3]["error"]["code"], -32601);
    assert_eq!(replies[4]["error"]["code"], -32700);
    for value in CANARY.iter().copied().chain(["me@example.com", SECRET]) {
        assert!(!text.contains(value), "{value}");
    }
}

#[test]
fn test_a_fill_finds_the_field_by_what_it_is_on_the_items_own_page() {
    let tmp = tempfile::tempdir().unwrap();
    let (v, ids) = setup(tmp.path());
    let answers: Mutex<Vec<String>> = Mutex::new(vec![]);
    let id_of = |which: &str| match which {
        "login" => ids.login.clone(),
        "card" => ids.card.clone(),
        _ => ids.totp.clone(),
    };
    let fill = |mac: Arc<FakeMac>, which: &str, part: &str, page: &str, label: &str| -> (bool, Vec<(String, String)>) {
        let mut server = Server::new(v.clone(), Some(mac.clone()), Box::new(io::sink()));
        server.field_waits = vec![0.0];
        server.count_wait = 0.0;
        let args = json!({"id": id_of(which), "part": part, "page": page, "field": label});
        let result = server.call(Some(&json!("vault_fill")), Some(&args));
        answers.lock().push(result["content"][0]["text"].as_str().unwrap().to_string());
        (!result["isError"].as_bool().unwrap(), mac.set())
    };
    let last = || answers.lock().last().cloned().unwrap();
    let pair = |w: &str, v: &str| vec![(w.to_string(), v.to_string())];

    let signin = vec![f("", LOGIN, false, 1, false), f("Password", LOGIN, true, 2, false)];
    let other_site = f("Password", "https://chase.com.evil.example/login", true, 1, false);
    let mut all = signin.clone();
    all.push(other_site.clone());
    let mac = Arc::new(FakeMac::new(all));
    assert_eq!(fill(mac.clone(), "login", "password", "", "").1, pair("Password", CANARY_PASSWORD));
    assert!(last().contains("into the password field on secure.chase.com"), "{}", last());
    assert_eq!(*mac.walked.lock(), HashSet::from([LOGIN.to_string()])); // the other site's page was never read
    // the field before the password
    assert_eq!(fill(Arc::new(FakeMac::new(signin.clone())), "login", "username", "", "").1, pair("", "me@example.com"));
    let named =
        vec![f("Search", LOGIN, false, 1, false), f("Email address", LOGIN, false, 2, false), f("Password", LOGIN, true, 3, false)];
    assert_eq!(fill(Arc::new(FakeMac::new(named)), "login", "username", "", "").1, pair("Email address", "me@example.com"));
    let focus =
        vec![f("Search", LOGIN, false, 1, false), f("Email address", LOGIN, false, 2, true), f("Password", LOGIN, true, 3, false)];
    assert_eq!(fill(Arc::new(FakeMac::new(focus)), "login", "username", "", "").1, pair("Email address", "me@example.com"));
    let pay = "https://shop.example/checkout";
    let card = vec![f("Card number", pay, false, 1, false), f("Expiry (MM/YY)", pay, false, 2, false), f("CVC", pay, false, 3, false)];
    assert_eq!(fill(Arc::new(FakeMac::new(card.clone())), "card", "number", pay, "").1, pair("Card number", CANARY_NUMBER));
    assert_eq!(
        fill(Arc::new(FakeMac::new(card.clone())), "card", "exp", &format!("{pay}#pay"), "").1,
        pair("Expiry (MM/YY)", "07/31")
    );
    assert_eq!(fill(Arc::new(FakeMac::new(card.clone())), "card", "cvc", pay, "").1, pair("CVC", CANARY_CVC));
    let framed = vec![
        f("Email", pay, false, 1, false),
        Field {
            pid: 1,
            app: "Google Chrome".into(),
            role: "AXTextField".into(),
            url: "https://js.payments.example/frame".into(),
            page: pay.into(),
            words: "Card number".into(),
            order: 2,
            ..Default::default()
        },
    ];
    assert_eq!(fill(Arc::new(FakeMac::new(framed)), "card", "number", pay, "").1, pair("Card number", CANARY_NUMBER));
    let login_framed = vec![Field {
        pid: 1,
        app: "Google Chrome".into(),
        role: "AXTextField".into(),
        subrole: "AXSecureTextField".into(),
        url: "https://evil.example/frame".into(),
        page: LOGIN.into(),
        words: "Password".into(),
        order: 1,
        ..Default::default()
    }];
    // a login never fills another site's frame
    assert!(!fill(Arc::new(FakeMac::new(login_framed)), "login", "password", "", "").0);
    assert_eq!(fill(Arc::new(FakeMac::new(vec![fw("Enter the code")])), "totp", "code", "", "").1, pair("Enter the code", &code()));
    let two_codes = vec![f("Code from SMS", LOGIN, false, 1, false), f("Backup code", LOGIN, false, 2, false)];
    assert_eq!(fill(Arc::new(FakeMac::new(two_codes.clone())), "totp", "code", "", "SMS").1, pair("Code from SMS", &code()));
    let two_pages = vec![f("Password", LOGIN, true, 1, false), f("Password", "https://chase.com/other", true, 1, false)];
    assert!(fill(Arc::new(FakeMac::new(two_pages.clone())), "login", "password", "https://chase.com/other", "").0);

    let refuses = FakeMac { takes: false, ..FakeMac::new(vec![f("Password", LOGIN, true, 1, false)]) };
    let refusals: Vec<(FakeMac, &str, &str, &str, &str, &str)> = vec![
        (FakeMac::new(vec![other_site.clone()]), "login", "password", "", "", "No page on chase.com with a field is in front"),
        (FakeMac::new(vec![f("Code", "https://example.com/", false, 1, false)]), "totp", "code", "", "", "No page on chase.com"),
        (FakeMac::new(vec![fw("Email")]), "login", "password", "", "", "only into a password field"),
        (FakeMac::new(signin.clone()), "login", "password", "", "Email", "No field labelled 'Email'"),
        (FakeMac::new(card.clone()), "card", "number", "", "", "Give the URL of the page"),
        (FakeMac::new(two_pages.clone()), "login", "password", "", "", "2 pages on chase.com are open"),
        (
            FakeMac::new(two_codes.clone()),
            "totp",
            "code",
            "",
            "",
            "several fields the code could go into ('Code from SMS', 'Backup code')",
        ),
        (refuses, "login", "password", "", "", "would not take a value"),
        (FakeMac::new(signin.clone()), "login", "cvc", "", "", "parts username, password"),
    ];
    for (mac, which, part, page, label, why) in refusals {
        let (ok, done) = fill(Arc::new(mac), which, part, page, label);
        assert!(!ok && done.is_empty() && last().contains(why), "{why:?} / {:?}", last());
    }
    let server = Server::new(v.clone(), Some(Arc::new(FakeMac::new(signin.clone()))), Box::new(io::sink()));
    let unknown = server.call(Some(&json!("vault_fill")), Some(&json!({"id": "v_zzzzzzzz", "part": "password"})));
    assert!(unknown["isError"].as_bool().unwrap());
    assert!(unknown["content"][0]["text"].as_str().unwrap().contains("No vault item"));
    let odd = Arc::new(FakeMac { count: Some(19), ..FakeMac::new(vec![f("Card number", pay, false, 1, false)]) });
    assert!(fill(odd, "card", "number", pay, "").0);
    assert!(last().contains("shows 19 characters where 16 were set"), "{}", last());
    let code = code();
    for text in answers.lock().iter() {
        for value in CANARY.iter().copied().chain(["me@example.com", code.as_str()]) {
            assert!(!text.contains(value), "{value:?} in {text:?}");
        }
    }
}

#[test]
fn test_codex_vault_server_config_carries_the_vault_settings() {
    let env = |k: &str| (k == "EVE_VAULT_INDEX").then(|| "/tmp/x/vault.json".to_string());
    assert_eq!(python_server_config("/py", &env)["env"], json!({"EVE_VAULT_INDEX": "/tmp/x/vault.json"}));
    assert_eq!(server_config_with("/bin/eve-vault-mcp", &env)["env"], json!({"EVE_VAULT_INDEX": "/tmp/x/vault.json"}));
}

#[test]
fn test_a_page_named_without_its_scheme_is_still_that_page() {
    // Review A12 (2026-10-18): Codex may name the page as "shop.com/checkout"; the scheme was compared exactly,
    // so no field ever matched and a card fill said no page was in front. A bare page reads as https, like
    // canonical_origin.
    let same = |a: &str, b: &str| same_page(a, b).unwrap();
    assert!(same("https://shop.com/checkout?step=2", "shop.com/checkout"));
    assert!(same("https://Shop.com/checkout/", "https://shop.com/checkout"));
    assert!(!same("https://shop.com/cart", "shop.com/checkout"));
    assert!(!same("http://shop.com/checkout", "shop.com/checkout")); // a bare page is https only
    assert!(!same("https://evil.com/checkout", "shop.com/checkout"));
}

#[test]
#[ignore = "live: drives a headed Chromium (Playwright) and the real Accessibility API; run the Python test with EVE_LIVE=1"]
fn test_live_native_fill_sets_the_right_fields_with_the_browser_behind_and_unfocused() {
    // The Python test needs Playwright to open the page and read the fields back; this crate has no browser
    // driver. It runs the server against the owner's real browsers: with no vault item, nothing is touched.
    let tmp = tempfile::tempdir().unwrap();
    let v = Arc::new(Vault::with_store(tmp.path().join("vault.json"), Box::new(MemKeychain::new())));
    let server = Server::new(v, None, Box::new(io::sink()));
    let r = server.call(Some(&json!("vault_fill")), Some(&json!({"id": "v_zzzzzzzz", "part": "password"})));
    assert!(r["isError"].as_bool().unwrap());
}

#[test]
#[ignore = "live: needs a headed Chromium with a payment frame (Playwright); run the Python test with EVE_LIVE=1"]
fn test_live_a_card_fills_a_payment_frame_from_another_host_inside_the_named_page() {
    // A real walk over the owner's browsers for a page nobody has open: refused as no page in front.
    let tmp = tempfile::tempdir().unwrap();
    let v = Arc::new(Vault::with_store(tmp.path().join("vault.json"), Box::new(MemKeychain::new())));
    let card = v
        .add("card", "Visa", &values(json!({"number": CANARY_NUMBER, "exp_month": "7", "exp_year": "2031", "cvc": "713"})), "")
        .unwrap()
        .handle;
    let mut server = Server::new(v, None, Box::new(io::sink()));
    server.field_waits = vec![0.0];
    let r = server.call(Some(&json!("vault_fill")), Some(&json!({"id": card, "part": "number", "page": "https://nowhere.invalid/x"})));
    let text = r["content"][0]["text"].as_str().unwrap();
    assert!(r["isError"].as_bool().unwrap() && !text.contains(CANARY_NUMBER), "{text}");
}

// ---- parity additions -----------------------------------------------------------------------------------------

#[test]
fn replies_are_written_as_python_json_dumps_writes_them() {
    let tmp = tempfile::tempdir().unwrap();
    let (v, _) = setup(tmp.path());
    let out = Buf::default();
    let server = Server::new(v, Some(Arc::new(FakeMac::new(vec![]))), Box::new(out.clone()));
    server.handle_line(r#"{"jsonrpc": "2.0", "id": 7, "method": "ping"}"#).unwrap();
    server.handle_line(r#"{"jsonrpc": "2.0", "id": 1.0, "method": "zapé"}"#).unwrap();
    assert_eq!(
        out.text(),
        "{\"jsonrpc\": \"2.0\", \"id\": 7, \"result\": {}}\n\
         {\"jsonrpc\": \"2.0\", \"id\": 1.0, \"error\": {\"code\": -32601, \"message\": \"method not found: zap\u{e9}\"}}\n"
    );
}

#[test]
fn initialize_answers_a_known_version_else_the_newest() {
    let tmp = tempfile::tempdir().unwrap();
    let (v, _) = setup(tmp.path());
    let out = Buf::default();
    let server = Server::new(v, Some(Arc::new(FakeMac::new(vec![]))), Box::new(out.clone()));
    for asked in [json!("2024-11-05"), json!("1999-01-01"), json!(5)] {
        server.handle(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": asked}})).unwrap();
    }
    server.handle(&json!({"jsonrpc": "2.0", "id": 2, "method": "initialize", "params": "x"})).unwrap();
    let replies: Vec<Value> = out.text().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let versions: Vec<&str> = replies.iter().map(|r| r["result"]["protocolVersion"].as_str().unwrap()).collect();
    assert_eq!(versions, ["2024-11-05", "2025-06-18", "2025-06-18", "2025-06-18"]);
    assert_eq!(
        replies[0]["result"],
        json!({"protocolVersion": "2024-11-05", "capabilities": {"tools": {"listChanged": false}},
               "serverInfo": {"name": "eve_vault", "version": "1.0.0"}, "instructions": INSTRUCTIONS})
    );
}

#[test]
fn invalid_requests_responses_notifications_and_batches_are_handled_as_python_does() {
    let tmp = tempfile::tempdir().unwrap();
    let (v, _) = setup(tmp.path());
    let out = Buf::default();
    let server = Server::new(v, Some(Arc::new(FakeMac::new(vec![]))), Box::new(out.clone()));
    server.handle_line(r#"{"id": 9, "result": {}}"#).unwrap(); // a response: silence
    server.handle_line(r#"{"jsonrpc": "2.0", "method": "ping"}"#).unwrap(); // a notification: silence
    server.handle_line(r#"{"jsonrpc": "1.0", "id": 3, "method": "ping"}"#).unwrap();
    server.handle_line(r#"{"jsonrpc": "2.0", "id": 4, "method": 5}"#).unwrap();
    server.handle_line("5").unwrap();
    server.handle_line(r#"[{"jsonrpc": "2.0", "id": 5, "method": "ping"}, 7]"#).unwrap();
    server.handle_line("[]").unwrap();
    server.handle_line("   ").unwrap();
    server.handle_line(r#"{"jsonrpc": "2.0", "id": null, "method": "ping"}"#).unwrap();
    let replies: Vec<Value> = out.text().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(
        replies,
        vec![
            json!({"jsonrpc": "2.0", "id": 3, "error": {"code": -32600, "message": "invalid request"}}),
            json!({"jsonrpc": "2.0", "id": 4, "error": {"code": -32600, "message": "invalid request"}}),
            json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32600, "message": "invalid request"}}),
            json!({"jsonrpc": "2.0", "id": 5, "result": {}}),
            json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32600, "message": "invalid request"}}),
            json!({"jsonrpc": "2.0", "id": null, "result": {}}),
        ]
    );
}

#[test]
fn serve_reads_lines_as_pythons_text_stdin_splits_them() {
    let tmp = tempfile::tempdir().unwrap();
    let (v, _) = setup(tmp.path());
    let out = Buf::default();
    let server = Server::new(v, Some(Arc::new(FakeMac::new(vec![]))), Box::new(out.clone()));
    let input = "{\"jsonrpc\": \"2.0\", \"id\": 1, \"method\": \"ping\"}\r\n\n{\"jsonrpc\": \"2.0\", \"id\": 2, \"method\": \"ping\"}\r{\"jsonrpc\": \"2.0\", \"id\": 3, \"method\": \"ping\"}";
    assert_eq!(server.serve(input.as_bytes()).unwrap(), 0);
    let ids: Vec<Value> = out.text().lines().map(|l| serde_json::from_str::<Value>(l).unwrap()["id"].clone()).collect();
    assert_eq!(ids, vec![json!(1), json!(2), json!(3)]);
}

#[test]
fn unknown_tools_odd_arguments_and_an_empty_vault_answer_in_sentences() {
    let tmp = tempfile::tempdir().unwrap();
    let empty = Arc::new(Vault::with_store(tmp.path().join("none.json"), Box::new(MemKeychain::new())));
    let server = Server::new(empty, Some(Arc::new(FakeMac::new(vec![]))), Box::new(io::sink()));
    let text = |r: Value| (r["isError"].as_bool().unwrap(), r["content"][0]["text"].as_str().unwrap().to_string());
    assert_eq!(text(server.call(Some(&json!("vault_list")), None)), (false, "The vault is empty.".to_string()));
    assert_eq!(text(server.call(Some(&json!("zap")), None)), (true, "No tool 'zap'.".to_string()));
    assert_eq!(text(server.call(None, None)), (true, "No tool 'None'.".to_string()));
    assert_eq!(text(server.call(Some(&json!(["a"])), None)), (true, "No tool \"['a']\".".to_string()));
    assert_eq!(
        text(server.call(Some(&json!("vault_fill")), Some(&json!({"id": 12345}))),),
        (true, "No vault item '12345'. vault_list shows the ids.".to_string())
    );
    assert_eq!(
        text(server.call(Some(&json!("vault_fill")), Some(&json!({"id": "  v_abcdefghijklmnopqrstuvwxyz  "})))),
        (true, "No vault item 'v_abcdefghijklmnopqr'. vault_list shows the ids.".to_string())
    );
}

#[test]
fn a_mac_that_cannot_be_made_or_is_not_trusted_is_said_plainly() {
    struct Untrusted;
    impl Native for Untrusted {
        fn trusted(&self) -> bool {
            false
        }
        fn page_fields(&self, _: &dyn Fn(&str) -> bool, _: bool) -> Vec<Field> {
            vec![]
        }
        fn set_value(&self, _: &Field, _: &str) -> bool {
            false
        }
        fn char_count(&self, _: &Field) -> Option<i64> {
            None
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let (v, ids) = setup(tmp.path());
    let args = json!({"id": ids.login, "part": "password"});
    let no_mac = Server::new(v.clone(), None, Box::new(io::sink()))
        .with_make_native(Box::new(|| Err(anyhow::anyhow!("the vault fills fields only on macOS"))));
    let r = no_mac.call(Some(&json!("vault_fill")), Some(&args));
    assert_eq!(r["content"][0]["text"], "The vault could not do that (RuntimeError).");
    // the refusals before the Mac is needed still answer
    let r = no_mac.call(Some(&json!("vault_fill")), Some(&json!({"id": ids.card, "part": "number"})));
    assert_eq!(r["content"][0]["text"], "Give the URL of the page the number goes into.");
    let untrusted = Server::new(v, Some(Arc::new(Untrusted)), Box::new(io::sink()));
    let r = untrusted.call(Some(&json!("vault_fill")), Some(&args));
    assert_eq!(r["content"][0]["text"], "eve has no Accessibility permission here, so it cannot reach the page.");
}

#[test]
fn a_page_urlsplit_refuses_is_a_value_error_as_in_python() {
    let tmp = tempfile::tempdir().unwrap();
    let (v, ids) = setup(tmp.path());
    let mut server = Server::new(v, Some(Arc::new(FakeMac::new(vec![fw("Card number")]))), Box::new(io::sink()));
    server.field_waits = vec![0.0];
    let r = server.call(Some(&json!("vault_fill")), Some(&json!({"id": ids.card, "part": "number", "page": "https://[::1/x"})));
    assert_eq!(r["content"][0]["text"], "The vault could not do that (ValueError).");
    assert!(same_page("https://a.example/x", "https://[::1/x").is_err());
}

#[test]
fn pick_field_messages_and_order_match_python() {
    let fields = vec![f("Zip", LOGIN, false, 3, false), f("", LOGIN, false, 2, false), f("Password", LOGIN, true, 1, false)];
    assert_eq!(pick_field(&fields, "password", "").unwrap().words, "Password");
    assert_eq!(pick_field(&fields, "password", "zip").unwrap_err(), "A password goes only into a password field, and the page has none with that label.");
    assert_eq!(pick_field(&fields, "number", "").unwrap_err(), "The page has several fields the number could go into ('unlabelled', 'Zip'). Give the field's label.");
    assert_eq!(pick_field(&fields[2..], "cvc", "").unwrap_err(), "The page has no field for the cvc.");
    assert_eq!(pick_field(&fields, "cvc", "Nope").unwrap_err(), "No field labelled 'Nope' on the page.");
    // casefold: "STRASSE" names the field called "Straße"
    let street = vec![f("Straße", LOGIN, false, 1, false), f("City", LOGIN, false, 2, false)];
    assert_eq!(pick_field(&street, "name", "STRASSE").unwrap().words, "Straße");
    // two focused fields decide nothing; the one whose words fit the part does
    let two_hot = vec![f("Card number", LOGIN, false, 1, true), f("Name on card", LOGIN, false, 2, true)];
    assert_eq!(pick_field(&two_hot, "name", "").unwrap().words, "Name on card");
    // a password prefers the focused secure field
    let two_pw = vec![f("Password", LOGIN, true, 1, false), f("Confirm", LOGIN, true, 2, true)];
    assert_eq!(pick_field(&two_pw, "password", "").unwrap().words, "Confirm");
}

#[test]
fn casefold_is_pythons() {
    assert_eq!(casefold("Straße"), "strasse");
    assert_eq!(casefold("ΣΊΣΥΦΟΣ"), "σίσυφοσ"); // no final sigma, unlike str::to_lowercase
    assert_eq!(casefold("ﬁle µ"), "file μ");
    assert_eq!(casefold("İ"), "i\u{307}");
}

#[test]
fn a_filled_field_is_named_by_its_words_or_as_text() {
    let tmp = tempfile::tempdir().unwrap();
    let (v, ids) = setup(tmp.path());
    let call = |fields: Vec<Field>, page: &str| {
        let mut server = Server::new(v.clone(), Some(Arc::new(FakeMac::new(fields))), Box::new(io::sink()));
        server.field_waits = vec![0.0];
        let r = server.call(Some(&json!("vault_fill")), Some(&json!({"id": ids.card, "part": "cvc", "page": page})));
        r["content"][0]["text"].as_str().unwrap().to_string()
    };
    let pay = "https://shop.example/pay";
    assert_eq!(call(vec![f("", pay, false, 1, false)], pay), "Filled the cvc of 'Visa' into the text field on shop.example.");
    assert_eq!(call(vec![f("CVC", pay, false, 1, false)], pay), "Filled the cvc of 'Visa' into the 'CVC' field on shop.example.");
    let odd = Field { url: "about:blank".into(), page: pay.into(), ..f("CVC", "", false, 1, false) };
    assert_eq!(call(vec![odd], pay), "Filled the cvc of 'Visa' into the 'CVC' field on about:blank.");
}

#[test]
fn tools_and_constants_are_pythons() {
    assert_eq!(TOOL_NAMES, ["vault_list", "vault_fill"]);
    assert_eq!(FIELD_WAITS, [0.0, 0.3, 0.5, 0.7, 1.0]);
    assert_eq!(COUNT_WAIT, 0.3);
    assert_eq!(
        dumps(&TOOLS[0]),
        r#"{"name": "vault_list", "description": "The owner's saved sign-ins, cards and 2FA secrets: one line each with its id, kind, label and the site it is for. Never a value.", "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}}"#
    );
    assert_eq!(
        dumps(&TOOLS[1]["inputSchema"]["properties"]["part"]),
        r#"{"type": "string", "enum": ["username", "password", "number", "exp", "exp_month", "exp_year", "cvc", "name", "code"]}"#
    );
}

#[test]
fn server_config_starts_this_binary_or_pythons_module() {
    let none = |_: &str| None;
    assert_eq!(server_config_with("/opt/eve/eve-vault-mcp", &none), json!({"command": "/opt/eve/eve-vault-mcp", "args": []}));
    assert!(!server_config_with("", &none)["command"].as_str().unwrap().is_empty());
    let both = |k: &str| Some(if k == "EVE_VAULT_INDEX" { "/i".to_string() } else { "/k".to_string() });
    assert_eq!(
        python_server_config("/py", &both),
        json!({"command": "/py", "args": ["-I", "-m", "eve_bridge.vault_mcp"], "env": {"EVE_VAULT_INDEX": "/i", "EVE_VAULT_KEYCHAIN": "/k"}})
    );
    let blank = |_: &str| Some(String::new());
    assert!(python_server_config("/py", &blank).get("env").is_none());
}

#[test]
fn tools_list_is_byte_for_byte_what_python_writes() {
    // Captured from the Python server (CPython 3.13, eve_bridge.vault_mcp) for this exact request.
    const PYTHON: &str = r#"{"jsonrpc": "2.0", "id": 2, "result": {"tools": [{"name": "vault_list", "description": "The owner's saved sign-ins, cards and 2FA secrets: one line each with its id, kind, label and the site it is for. Never a value.", "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}}, {"name": "vault_fill", "description": "Put one part of a saved item into a field of the page, without you seeing it. Give the page's URL, and the field's label when the page has several fields of that kind. Parts: a login has username and password; a card has number, exp (MM/YY), exp_month, exp_year, cvc and name; a 2FA item has code (generated now). A login or 2FA item fills only on its own site; a password fills only into a password field.", "inputSchema": {"type": "object", "additionalProperties": false, "required": ["id", "part"], "properties": {"id": {"type": "string", "description": "The item's id from vault_list"}, "part": {"type": "string", "enum": ["username", "password", "number", "exp", "exp_month", "exp_year", "cvc", "name", "code"]}, "page": {"type": "string", "description": "The URL of the page the field is on"}, "field": {"type": "string", "description": "The field's label, when needed"}}}}]}}"#;
    let tmp = tempfile::tempdir().unwrap();
    let (v, _) = setup(tmp.path());
    let out = Buf::default();
    let server = Server::new(v, Some(Arc::new(FakeMac::new(vec![]))), Box::new(out.clone()));
    server.handle_line(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#).unwrap();
    assert_eq!(out.text(), format!("{PYTHON}\n"));
}
