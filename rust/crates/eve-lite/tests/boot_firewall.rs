//! test_lite_boot_firewall.py: eve lite's daemon boots under its own process firewall and trips it nowhere.
//!
//! Each boot is `run_daemon` in a fresh child process, as launchd runs it: the lite LaunchAgent's PATH (with a folder
//! of fake developer tools first, each of which leaves a mark if it ever runs), a fake OpenRouter key, a temporary
//! HOME. run_daemon installs the firewall in enforce mode before it builds anything (it cannot be removed, so this
//! test process never installs one). Everything is the real part, with three changes: the page and the LAN door listen
//! on ephemeral loopback ports, the network is cut (every HTTP client goes through a refusing proxy that records the
//! host), and the OpenRouter exchange of the sign-in boot answers from a mock transport.
//!
//! * READY: memory, the chat log, the watcher, Composio's bridge (a token file is there), the brain, the chat door,
//!   the calls, WebRTC, the page, the LAN door (web.json says it was on), the IPC socket, the spend figures and the
//!   dream. A browser signs in and reads its status. Then SIGTERM, and exit 0.
//! * NEEDS_OPENROUTER, then the sign-in: the page's OpenRouter callback saves the key (0600) and eve exits 75.
//!
//! Both must hold: zero refused process starts and zero protected-folder accesses (firewall_counts is {} at READY
//! and after the stop), no fake developer tool ever ran, and the only hosts eve tried are the ones the spec says leave
//! the Mac (M9). Positive controls, in the same process once the daemon stopped: a harmless program and a file in
//! ~/Documents are refused, so the firewall really was on and enforcing.

mod common;

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use common::*;
use eve_lite::{LiteDaemon, LiteOptions};
use parking_lot::Mutex;
use serde_json::{json, Map, Value};

const SHIMS: &[&str] = &[
    "git", "python3", "clang", "cc", "make", "xcrun", "install_name_tool", "osascript", "screencapture", "codex",
    "claude", "cloudflared", "adb",
];
/// what the spec lets leave the Mac (M9)
const EGRESS: &[&str] = &["openrouter.ai", "connect.composio.dev", "login.composio.dev"];
const REPORT: &str = "eve-lite boot report:";

// ---- the child ---------------------------------------------------------------------------------------------------

fn ipc(req: Value) -> Value {
    let sock = std::env::var("EVE_BRIDGE_SOCK").unwrap();
    let mut s = UnixStream::connect(sock).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    s.write_all((req.to_string() + "\n").as_bytes()).unwrap();
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

/// One request to the page: status, headers (lowercase), body, Set-Cookie lines.
fn web(port: u64, method: &str, path: &str, body: Option<Value>, cookie: &str) -> (u16, Map<String, Value>, Vec<u8>, Vec<String>) {
    let host = format!("localhost:{port}");
    let mut lines = vec![
        format!("{method} {path} HTTP/1.1"),
        format!("Host: {host}"),
        "User-Agent: Mozilla/5.0 (Macintosh) AppleWebKit/605.1.15 Version/18.0 Safari/605.1.15".into(),
        "Connection: close".into(),
    ];
    let data = body.map(|b| b.to_string().into_bytes()).unwrap_or_default();
    if !data.is_empty() {
        lines.push(format!("Origin: http://{host}"));
        lines.push("Content-Type: application/json".into());
        lines.push(format!("Content-Length: {}", data.len()));
    }
    if !cookie.is_empty() {
        lines.push(format!("Cookie: {cookie}"));
    }
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port as u16)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    let mut raw = (lines.join("\r\n") + "\r\n\r\n").into_bytes();
    raw.extend_from_slice(&data);
    stream.write_all(&raw).unwrap();
    let mut got = Vec::new();
    stream.read_to_end(&mut got).unwrap();
    let split = got.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8_lossy(&got[..split]).into_owned();
    let mut head_lines = head.split("\r\n");
    let status = head_lines.next().unwrap().split(' ').nth(1).unwrap().parse().unwrap();
    let (mut headers, mut cookies) = (Map::new(), Vec::new());
    for line in head_lines {
        let (k, v) = line.split_once(':').unwrap_or((line, ""));
        let k = k.trim().to_lowercase();
        if k == "set-cookie" {
            cookies.push(v.trim().to_string());
        }
        headers.insert(k, json!(v.trim()));
    }
    (status, headers, got[split + 4..].to_vec(), cookies)
}

fn mac_cookie(port: u64, report: &mut Map<String, Value>) -> String {
    let login = ipc(json!({"evt": "web_login"}));
    let (status, _, _, cookies) = web(port, "POST", eve_lite_web::MAC_LOGIN_PATH, Some(json!({"code": login["code"]})), "");
    report.insert("signed_in".into(), json!(status));
    cookies[0].split(';').next().unwrap().to_string()
}

fn check_ready(d: &LiteDaemon, report: &mut Map<String, Value>) {
    let status = ipc(json!({"evt": "status"}));
    report.insert("status".into(), status.clone());
    let port = status["port"].as_u64().unwrap();
    let health: Value = serde_json::from_slice(&web(port, "GET", eve_lite_web::HEALTH_PATH, None, "").2).unwrap();
    report.insert("health".into(), health);
    report.insert("index".into(), json!(web(port, "GET", "/", None, "").0));
    let cookie = mac_cookie(port, report);
    report.insert("web_status".into(), json!(web(port, "GET", eve_lite_web::STATUS_PATH, None, &cookie).0));
    report.insert("devices".into(), json!(ipc(json!({"evt": "devices"}))["devices"].as_array().unwrap().len()));
    report.insert("state".into(), json!(d.state()));
    report.insert("ready_counts".into(), json!(eve_profile::firewall::firewall_counts()));
    report.insert("sock_mode".into(), json!(mode(std::path::Path::new(&std::env::var("EVE_BRIDGE_SOCK").unwrap()))));
}

fn check_sign_in(d: &LiteDaemon, report: &mut Map<String, Value>) {
    let status = ipc(json!({"evt": "status"}));
    report.insert("status".into(), status.clone());
    let port = status["port"].as_u64().unwrap();
    let cookie = mac_cookie(port, report);
    let start = web(port, "GET", eve_lite_web::OPENROUTER_START_PATH, None, &cookie);
    let location = start.1["location"].as_str().unwrap().to_string();
    let state = location.split_once('?').unwrap().1.split('&').find_map(|kv| kv.strip_prefix("state=")).unwrap().to_string();
    let path = format!("{}?code=code-1234&state={state}", eve_lite_web::OPENROUTER_CALLBACK_PATH);
    report.insert("callback".into(), json!(web(port, "GET", &path, None, "").0));
    report.insert("state".into(), json!(d.state()));
    report.insert("ready_counts".into(), json!(eve_profile::firewall::firewall_counts()));
}

struct Provider;

#[async_trait::async_trait]
impl eve_brain::seams::HttpTransport for Provider {
    async fn send(
        &self,
        request: eve_brain::seams::HttpRequest,
    ) -> Result<eve_brain::seams::HttpResponse, eve_brain::seams::HttpError> {
        if request.url == eve_brain::onboarding::OPENROUTER_KEYS_URL {
            return Ok(eve_brain::seams::HttpResponse {
                status: 200,
                headers: vec![("content-type".into(), "application/json".into())],
                body: json!({"key": CANARY_KEY}).to_string().into_bytes(),
            });
        }
        Ok(eve_brain::seams::HttpResponse { status: 404, headers: vec![], body: vec![] })
    }
}

/// The boot, in the child: run_daemon with the checks run from on_ready, then the controls; the report on stderr.
fn boot_child(name: &str, flow: &'static str) -> bool {
    if !is_child(name) {
        return false;
    }
    child_start(name);
    // as cli.main does, right after the env file
    eve_profile::apply_process_env().expect("apply");
    let report = Arc::new(Mutex::new(Map::new()));
    let kept = report.clone();
    let on_ready: eve_lite::OnReady = Arc::new(move |d: &Arc<LiteDaemon>| {
        let (d, kept) = (d.clone(), kept.clone());
        std::thread::spawn(move || {
            let mut r = Map::new();
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match flow {
                "ready" => check_ready(&d, &mut r),
                _ => check_sign_in(&d, &mut r),
            }));
            if outcome.is_err() {
                r.insert("check_error".into(), json!("the check panicked"));
            }
            let failed = outcome.is_err();
            kept.lock().extend(r);
            if flow == "ready" || failed {
                // eve restarts by itself after the sign-in; this only ends a ready boot or a failure
                unsafe {
                    libc::kill(libc::getpid(), libc::SIGTERM);
                }
            }
        });
        Ok(())
    });
    let web: eve_lite::WebOptions = Arc::new(|p: &mut eve_lite_web::Parts| {
        p.local_ports = vec![0];
        p.lan_ports = vec![0];
        p.lan_hosts = vec!["127.0.0.1".into()];
        p.restart_delay = 0.0;
    });
    let mut options = LiteOptions { web_options: Some(web), on_ready: Some(on_ready), ..LiteOptions::default() };
    if flow == "sign-in" {
        options.openrouter_login =
            Some(Arc::new(eve_brain::onboarding::OpenRouterLogin::new(Some(Arc::new(Provider)), None)));
    }
    let code = eve_lite::run_daemon(None, options);
    let mut r = report.lock().clone();
    r.insert("exit".into(), json!(code));
    r.insert("stopped_counts".into(), json!(eve_profile::firewall::firewall_counts()));
    r.insert("sock_left".into(), json!(std::path::Path::new(&std::env::var("EVE_BRIDGE_SOCK").unwrap()).exists()));
    // the positive controls: the firewall is on and enforcing
    let ran = eve_profile::firewall::status(&mut Command::new("/usr/bin/true")).is_ok();
    r.insert("control_exec".into(), json!(if ran { "ran" } else { "refused" }));
    let private = home().join("Documents").join("private.txt");
    let opened = eve_profile::firewall::audit_path(
        eve_profile::firewall::FileEvent::Open,
        [private.to_string_lossy().into_owned()],
    )
    .and_then(|()| std::fs::read_to_string(&private).map(|_| ()));
    r.insert("control_open".into(), json!(if opened.is_ok() { "read" } else { "refused" }));
    r.insert("control_counts".into(), json!(eve_profile::firewall::firewall_counts()));
    eprintln!("{REPORT} {}", Value::Object(r));
    true
}

// ---- the parent --------------------------------------------------------------------------------------------------

struct Boot {
    b: Box_,
    bin: std::path::PathBuf,
    marks: std::path::PathBuf,
}

fn boot_box() -> Boot {
    let b = make_box();
    let (bin, marks) = (b.tmp.path().join("bin"), b.tmp.path().join("marks"));
    for folder in [b.home.join("Documents"), bin.clone(), marks.clone()] {
        std::fs::create_dir_all(folder).unwrap();
    }
    std::fs::write(b.home.join("Documents/private.txt"), "private\n").unwrap();
    for name in SHIMS {
        let fake = bin.join(name);
        std::fs::write(&fake, format!("#!/bin/sh\ntouch \"{}/{name}\"\n", marks.display())).unwrap();
        set_mode(&fake, 0o755);
    }
    Boot { b, bin, marks }
}

fn boot(boot: &Boot, name: &str, key: bool) -> (Value, String, Vec<String>) {
    let child = Child {
        key,
        profile: Some("lite".into()),
        path: Some(format!("{}:{LITE_PATH}", boot.bin.display())),
        env: vec![("EVE_BRIDGE_SOCK".into(), boot.b.sock.to_string_lossy().into_owned())],
        apply_profile: false,
    };
    let (asked, log) = run_child(name, &boot.b, child);
    let line = log.lines().rev().find_map(|l| l.strip_prefix(REPORT)).expect("a report");
    let report: Value = serde_json::from_str(line.trim()).unwrap();
    assert!(report.get("check_error").is_none(), "{report}\n{}", &log[log.len().saturating_sub(4000)..]);
    let hosts: BTreeSet<String> = asked.hosts().into_iter().collect();
    eprintln!("{name}: hosts asked: {hosts:?}");
    (report, log, hosts.into_iter().collect())
}

/// Nothing refused at READY or by the stop, no fake tool ran, nothing but the spec's hosts was tried; and the controls
/// show the firewall was enforcing all along.
fn assert_clean(report: &Value, boot: &Boot, log: &str, asked: &[String]) {
    assert_eq!((report["ready_counts"].clone(), report["stopped_counts"].clone()), (json!({}), json!({})), "{report}");
    assert!(!log.split("lite: stopped").next().unwrap().contains("firewall: refused"));
    assert_eq!(std::fs::read_dir(&boot.marks).unwrap().count(), 0, "a developer tool ran");
    assert!(asked.iter().all(|h| EGRESS.contains(&h.as_str())), "{asked:?}");
    assert_eq!(report["sock_left"], json!(false));
    assert!(!log.contains(CANARY_KEY));
    // positive controls
    assert_eq!((report["control_exec"].as_str(), report["control_open"].as_str()), (Some("refused"), Some("refused")));
    let counts = report["control_counts"].as_object().unwrap();
    assert!(counts.contains_key("exec:true") && counts.contains_key("path:~/Documents"), "{counts:?}");
    assert!(log.contains("firewall: on (enforce)"));
}

#[test]
fn a_ready_boot_trips_the_firewall_nowhere() {
    const NAME: &str = "a_ready_boot_trips_the_firewall_nowhere";
    if boot_child(NAME, "ready") {
        return;
    }
    let bx = boot_box();
    let config = bx.b.home.join(".config/eve-bridge");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("web.json"), json!({"version": 1, "lan": true}).to_string()).unwrap(); // a phone was added
    std::fs::write(
        config.join("composio-connect.json"),
        json!({"version": 1, "client_id": "eve-lite-test-client", "access_token": "placeholder-access",
               "refresh_token": "placeholder-refresh", "expires_at": 4102444800_i64})
        .to_string(),
    )
    .unwrap();
    set_mode(&config.join("composio-connect.json"), 0o600);
    let (report, log, asked) = boot(&bx, NAME, true);
    assert_eq!(report["exit"], json!(0), "{report}");
    assert_eq!(report["state"], json!("READY"));
    let status = &report["status"];
    assert_eq!(status["state"], json!("READY"));
    assert_eq!((status["openrouter"].clone(), status["composio"].clone()), (json!(true), json!(true)));
    assert_eq!((status["page"].clone(), status["lan"].clone(), status["voice"].clone()), (json!(true), json!(true), json!(true)));
    assert!(status["lan_port"].as_u64().unwrap() > 0);
    assert_eq!(status["firewall"], json!({}));
    assert_eq!(report["health"], json!({"ok": true, "state": "READY"}));
    assert_eq!(report["index"], json!(200));
    assert_eq!((report["signed_in"].clone(), report["web_status"].clone()), (json!(200), json!(200)));
    assert_eq!(report["devices"], json!(1));
    assert_eq!(report["sock_mode"], json!(0o600));
    assert_clean(&report, &bx, &log, &asked);
}

#[test]
fn a_setup_boot_signs_in_and_restarts_with_exit_75() {
    const NAME: &str = "a_setup_boot_signs_in_and_restarts_with_exit_75";
    if boot_child(NAME, "sign-in") {
        return;
    }
    let bx = boot_box();
    let (report, log, asked) = boot(&bx, NAME, false);
    assert_eq!(report["status"]["state"], json!("NEEDS_OPENROUTER"));
    assert_eq!(report["status"]["openrouter"], json!(false));
    assert_eq!(report["state"], json!("NEEDS_OPENROUTER"));
    assert_eq!(report["callback"], json!(303));
    assert_eq!(report["exit"], json!(75)); // launchd starts it again, into READY
    let env_file = bx.b.home.join(".config/eve-bridge/env");
    assert!(read(&env_file).lines().any(|l| l == format!("OPENROUTER_API_KEY={CANARY_KEY}")));
    assert_eq!(mode(&env_file), 0o600);
    assert_eq!(asked, Vec::<String>::new()); // the exchange went to the stand-in only
    assert_clean(&report, &bx, &log, &asked);
}
