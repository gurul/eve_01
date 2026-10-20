//! Port of bridge/tests/test_vault_mcp.py: the vault's two tools for Codex tasks and which field a value may go
//! into. The Codex wiring tests there (`_thread_params`, `_vault_changed`, `vault_tool_call`) test
//! codex_computer and belong to its crate; `server_config` is tested here.

use std::collections::HashSet;
use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;

use eve_vault::{totp_default, MemKeychain, Values, Vault};
use eve_vault_native::{Field, Native, NativeMac};
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

// ---- live: the real Mac ------------------------------------------------------------------------------------------

/// What the Python live tests did with Playwright, done over the Chrome DevTools Protocol: a static file server
/// (Python's `http.server`), a headed Chrome for Testing (Playwright's Chromium) in a fresh profile, a window per
/// page, and `evaluate` in a page or in its out-of-process frame.
mod live {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use serde_json::{json, Value};
    use tokio_tungstenite::tungstenite::{self, Message, WebSocket};

    /// Serves `dir` on 127.0.0.1 at a free port; returns the port.
    pub fn serve_dir(dir: PathBuf) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let dir = dir.clone();
                std::thread::spawn(move || answer(conn, &dir));
            }
        });
        port
    }

    fn answer(mut conn: TcpStream, dir: &Path) {
        let Ok(clone) = conn.try_clone() else { return };
        let mut reader = BufReader::new(clone);
        let mut first = String::new();
        if reader.read_line(&mut first).is_err() {
            return;
        }
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                break;
            }
        }
        let path = first.split_whitespace().nth(1).unwrap_or("/");
        let path = path.split(['?', '#']).next().unwrap_or("").trim_start_matches('/');
        let body = if path.is_empty() || path.contains("..") { None } else { std::fs::read(dir.join(path)).ok() };
        let (status, body) = match body {
            Some(b) => ("200 OK", b),
            None => ("404 Not Found", b"not found".to_vec()),
        };
        let head = format!(
            "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = conn.write_all(head.as_bytes());
        let _ = conn.write_all(&body);
    }

    /// `EVE_LIVE_CHROME`, else Playwright's newest Chrome for Testing, else the installed Google Chrome.
    fn binary() -> PathBuf {
        if let Ok(path) = std::env::var("EVE_LIVE_CHROME") {
            return path.into();
        }
        let home = std::env::var("HOME").unwrap_or_default();
        let cache = Path::new(&home).join("Library/Caches/ms-playwright");
        let mut found: Vec<PathBuf> = std::fs::read_dir(&cache)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("chromium-"))
            .flat_map(|e| {
                ["chrome-mac-arm64", "chrome-mac"].map(|d| {
                    e.path().join(d).join("Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing")
                })
            })
            .filter(|p| p.exists())
            .collect();
        found.sort();
        found.pop().unwrap_or_else(|| "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome".into())
    }

    pub struct Chrome {
        child: Child,
        port: u16,
        _profile: tempfile::TempDir,
    }

    impl Drop for Chrome {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    fn deadline_loop<T>(what: &str, mut attempt: impl FnMut() -> Option<T>) -> T {
        let until = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(got) = attempt() {
                return got;
            }
            assert!(Instant::now() < until, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    impl Chrome {
        /// Chrome for Testing builds its web Accessibility tree only when forced; the owner's Chrome had it on
        /// (eve-vault-native's docs).
        pub fn launch(headless: bool) -> Chrome {
            let profile = tempfile::tempdir().unwrap();
            let mut cmd = Command::new(binary());
            cmd.arg("--remote-debugging-port=0")
                .arg(format!("--user-data-dir={}", profile.path().display()))
                .args(["--no-first-run", "--no-default-browser-check", "--force-renderer-accessibility"]);
            if headless {
                cmd.arg("--headless=new");
            }
            cmd.arg("about:blank").stdout(Stdio::null()).stderr(Stdio::null());
            let child = cmd.spawn().expect("start Chrome");
            let file = profile.path().join("DevToolsActivePort");
            let port = deadline_loop("DevToolsActivePort", || {
                std::fs::read_to_string(&file).ok()?.lines().next()?.trim().parse::<u16>().ok()
            });
            Chrome { child, port, _profile: profile }
        }

        fn http_json(&self, path: &str) -> Option<Value> {
            // DevTools keeps the connection open whatever the request says: read Content-Length bytes.
            let mut conn = TcpStream::connect(("127.0.0.1", self.port)).ok()?;
            conn.set_read_timeout(Some(Duration::from_secs(10))).ok()?;
            write!(conn, "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n", self.port).ok()?;
            let mut reader = BufReader::new(conn);
            let mut length = 0;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).ok()? == 0 {
                    return None;
                }
                if line.trim().is_empty() {
                    break;
                }
                if let Some((name, v)) = line.split_once(':') {
                    if name.eq_ignore_ascii_case("content-length") {
                        length = v.trim().parse().ok()?;
                    }
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).ok()?;
            serde_json::from_slice(&body).ok()
        }

        /// Playwright's `browser.new_page()` + `goto(url)`: a page in a window of its own.
        pub fn new_window(&self, url: &str) {
            let version = deadline_loop("/json/version", || self.http_json("/json/version"));
            let mut browser = Cdp::connect(version["webSocketDebuggerUrl"].as_str().unwrap());
            browser.call("Target.createTarget", json!({"url": url, "newWindow": true}));
        }

        /// The page or frame target whose URL is `url`, once it has loaded and `ready` holds in it.
        pub fn target(&self, kind: &str, url: &str, ready: &str) -> Cdp {
            deadline_loop(url, || {
                let list = self.http_json("/json/list")?;
                let t = list.as_array()?.iter().find(|t| t["type"] == kind && t["url"] == url)?.clone();
                let mut cdp = Cdp::connect(t["webSocketDebuggerUrl"].as_str()?);
                (cdp.eval(ready) == json!(true)).then_some(cdp)
            })
        }
    }

    pub struct Cdp {
        ws: WebSocket<TcpStream>,
        next: u64,
    }

    impl Cdp {
        fn connect(url: &str) -> Cdp {
            let host = url.trim_start_matches("ws://").split('/').next().unwrap();
            let stream = TcpStream::connect(host).unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
            let (ws, _) = tungstenite::client(url, stream).expect("DevTools websocket");
            Cdp { ws, next: 0 }
        }

        fn call(&mut self, method: &str, params: Value) -> Value {
            self.next += 1;
            let id = self.next;
            self.ws.send(Message::text(json!({"id": id, "method": method, "params": params}).to_string())).unwrap();
            loop {
                let Message::Text(text) = self.ws.read().unwrap() else { continue };
                let msg: Value = serde_json::from_str(text.as_str()).unwrap();
                if msg["id"] == json!(id) {
                    assert!(msg.get("error").is_none(), "{method}: {msg}");
                    return msg["result"].clone();
                }
            }
        }

        /// `page.evaluate(expression)`.
        pub fn eval(&mut self, expression: &str) -> Value {
            let r = self.call("Runtime.evaluate", json!({"expression": expression, "returnByValue": true, "awaitPromise": true}));
            r["result"]["value"].clone()
        }
    }

    /// The bundle id of the app in front (Python's NSWorkspace frontmostApplication().bundleIdentifier()).
    pub fn front_bundle() -> String {
        let asn = Command::new("lsappinfo").arg("front").output().unwrap();
        let asn = String::from_utf8_lossy(&asn.stdout).trim().to_string();
        let info = Command::new("lsappinfo").args(["info", "-only", "bundleid", &asn]).output().unwrap();
        let info = String::from_utf8_lossy(&info.stdout).to_string();
        info.lines()
            .find(|l| l.contains("bundleID=") || l.contains("\"CFBundleIdentifier\"="))
            .and_then(|l| l.split_once('='))
            .map(|(_, v)| v.trim().trim_matches('"').to_string())
            .unwrap_or_default()
    }
}

const PAGE: &str = r#"<!doctype html><html><head><title>Sign in</title></head><body style="margin:40px">
<form><input id=user placeholder="Email" style="width:300px;height:30px"><br><br>
<input id=pw type=password placeholder="Password" style="width:300px;height:30px"></form>
<script>window.ev = []; for (const id of ['user', 'pw']) for (const t of ['input', 'change'])
  document.getElementById(id).addEventListener(t, () => ev.push(id + ':' + t));</script></body></html>"#;

#[test]
#[ignore = "live: opens a headed Chrome for Testing with Finder in front and fills it through the Accessibility API (EVE_LIVE=1 in Python)"]
fn test_live_native_fill_sets_the_right_fields_with_the_browser_behind_and_unfocused() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("login.html"), PAGE).unwrap();
    let port = live::serve_dir(tmp.path().to_path_buf());
    let (here_url, there_url) =
        (format!("http://127.0.0.1:{port}/login.html"), format!("http://localhost:{port}/login.html"));
    let v = Arc::new(Vault::with_store(tmp.path().join("vault.json"), Box::new(MemKeychain::new())));
    let login = |label: &str, user: &str, pw: &str, site: &str| {
        v.add("login", label, &values(json!({"username": user, "password": pw})), site).unwrap().handle
    };
    let here = login("Local", "me@example.com", CANARY_PASSWORD, &format!("http://127.0.0.1:{port}"));
    let other = login("Elsewhere", "x", "elsewhere-pw", "other.example");
    let server = Server::new(v.clone(), Some(Arc::new(NativeMac::new().unwrap())), Box::new(io::sink()));
    let fill = |h: &str, part: &str, label: &str| {
        server.call(Some(&json!("vault_fill")), Some(&json!({"id": h, "part": part, "page": here_url, "field": label})))
    };
    let chrome = live::Chrome::launch(false);
    // Two windows: the item's site, and the same page on another host.
    chrome.new_window(&here_url);
    chrome.new_window(&there_url);
    let loaded = "document.readyState === 'complete' && !!document.getElementById('pw')";
    let mut here_page = chrome.target("page", &here_url, loaded);
    let mut there_page = chrome.target("page", &there_url, loaded);
    // nothing focused, and another app in front, as a Codex task leaves the browser
    for pg in [&mut here_page, &mut there_page] {
        pg.eval("document.activeElement && document.activeElement.blur(), true");
    }
    // activation from here is ignored; LaunchServices is not
    assert!(std::process::Command::new("open").args(["-a", "Finder"]).status().unwrap().success());
    std::thread::sleep(std::time::Duration::from_secs(1));
    let front = live::front_bundle();
    let first = fill(&other, "password", ""); // another site's login: refused
    let second = fill(&here, "password", "Email"); // a password into the Email field: refused
    let third = fill(&here, "username", "");
    let fourth = fill(&here, "password", "");
    let value = |pg: &mut live::Cdp, id: &str| pg.eval(&format!("document.getElementById('{id}').value"));
    let here_user = value(&mut here_page, "user");
    let here_pw_ok = here_page.eval(&format!("document.getElementById('pw').value === {}", json!(CANARY_PASSWORD)));
    let there = [value(&mut there_page, "user"), value(&mut there_page, "pw")];
    let events = here_page.eval("ev");
    let still = live::front_bundle();
    drop(chrome);
    let text = |r: &Value| r["content"][0]["text"].as_str().unwrap().to_string();
    assert!(first["isError"].as_bool().unwrap() && text(&first).contains("No page on other.example"), "{first}");
    assert!(second["isError"].as_bool().unwrap() && text(&second).contains("only into a password field"), "{second}");
    assert!(!third["isError"].as_bool().unwrap() && !fourth["isError"].as_bool().unwrap(), "{third} {fourth}");
    assert_eq!(here_user, json!("me@example.com"));
    assert_eq!(here_pw_ok, json!(true));
    assert_eq!(there, [json!(""), json!("")]); // the same form on another host: untouched
    assert_eq!(events, json!(["user:input", "user:change", "pw:input", "pw:change"]));
    // the browser never came to the front
    assert_eq!((front.as_str(), still.as_str()), ("com.apple.finder", "com.apple.finder"));
    for r in [&first, &second, &third, &fourth] {
        assert!(!r.to_string().contains(CANARY_PASSWORD) && !r.to_string().contains("elsewhere-pw"), "{r}");
    }
}

const CHECKOUT: &str = r#"<!doctype html><html><head><title>Checkout</title></head><body style="margin:40px">
<label for=email>Email</label><input id=email style="width:300px;height:30px">
<iframe id=pay src="FRAME" style="width:420px;height:120px;border:0"></iframe></body></html>"#;
const CARD_FRAME: &str = r#"<!doctype html><html><body><label for=cc>Card number</label>
<input id=cc style="width:300px;height:30px"></body></html>"#;

#[test]
#[ignore = "live: opens a headed Chrome for Testing with a payment frame from another host and fills it through the Accessibility API (EVE_LIVE=1 in Python)"]
fn test_live_a_card_fills_a_payment_frame_from_another_host_inside_the_named_page() {
    let tmp = tempfile::tempdir().unwrap();
    let port = live::serve_dir(tmp.path().to_path_buf());
    let frame = format!("http://localhost:{port}/frame.html");
    std::fs::write(tmp.path().join("frame.html"), CARD_FRAME).unwrap();
    std::fs::write(tmp.path().join("checkout.html"), CHECKOUT.replace("FRAME", &frame)).unwrap();
    let checkout = format!("http://127.0.0.1:{port}/checkout.html");
    let v = Arc::new(Vault::with_store(tmp.path().join("vault.json"), Box::new(MemKeychain::new())));
    let card = v
        .add("card", "Visa", &values(json!({"number": CANARY_NUMBER, "exp_month": "7", "exp_year": "2031", "cvc": "713"})), "")
        .unwrap()
        .handle;
    let server = Server::new(v, Some(Arc::new(NativeMac::new().unwrap())), Box::new(io::sink()));
    let chrome = live::Chrome::launch(false);
    chrome.new_window(&checkout);
    let mut page = chrome.target("page", &checkout, "document.readyState === 'complete'");
    // page.frame_locator("#pay").locator("#cc").wait_for(): the frame is another site, so its own target
    let mut pay = chrome.target("iframe", &frame, "!!document.getElementById('cc')");
    let result = server.call(Some(&json!("vault_fill")), Some(&json!({"id": card, "part": "number", "page": checkout})));
    let in_frame = pay.eval(&format!("document.getElementById('cc').value === {}", json!(CANARY_NUMBER)));
    let email = page.eval("document.getElementById('email').value");
    drop(chrome);
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(!result["isError"].as_bool().unwrap() && !text.contains("shows"), "{result}");
    assert_eq!(in_frame, json!(true));
    assert_eq!(email, json!(""));
    assert!(!result.to_string().contains(CANARY_NUMBER));
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

#[test]
fn part_words_match_as_pythons_re_does() {
    // Python's `\b` beside "pan": `²` and `½` are numbers (word characters), a combining mark is not.
    // Expected answers from CPython 3.13 `re.search(r"\bpan\b", s, re.I)`.
    for (words, fits) in [
        ("PAN", true),
        ("Card PAN:", true),
        ("pan\u{b2}", false),
        ("\u{bd}pan", false),
        ("pan\u{301}", true),
        ("\u{300}pan", true),
        ("pan_", false),
        ("span", false),
        ("pan-1", true),
    ] {
        assert_eq!(words_fit("number", words), fits, "{words:?}");
    }
    // `re.I` matches the pattern's `i` to `İ` and `ı` (Turkish), which simple case folding does not.
    assert!(words_fit("username", "LOG\u{130}N"));
    assert!(words_fit("code", "VER\u{130}F\u{130}CAT\u{130}ON"));
    assert!(words_fit("exp", "EXP\u{130}R"));
    assert!(words_fit("exp", "exp\u{131}r"));
    assert!(!words_fit("password", "Password"));
    assert!(!words_fit("exp_month", "Month"));
}

#[test]
fn lower_is_pythons_and_keeps_what_python_does_not_know() {
    assert_eq!(lower("Shop.COM"), "shop.com");
    assert_eq!(lower("\u{391}\u{3a3} \u{391}\u{3a3}\u{391}"), "\u{3b1}\u{3c2} \u{3b1}\u{3c3}\u{3b1}"); // final sigma, as str.lower
    assert_eq!(lower("\u{130}"), "i\u{307}");
    // unassigned in Unicode 15.1 (CPython 3.13): lower() and casefold() keep them
    assert_eq!(lower("A\u{a7cb}B"), "a\u{a7cb}b");
    assert_eq!(casefold("\u{1c89}x"), "\u{1c89}x");
    assert!(same_page("https://SHOP.example/x", "shop.example/x").unwrap());
}
