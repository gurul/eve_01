//! test_lite_web_local.py: the Mac's own page (LocalServer) on a loopback port.

mod common;

use std::io::Read as _;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use common::*;
use eve_lite_web::*;
use futures::FutureExt;
use serde_json::{json, Value};

fn mode(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn read_json(path: &std::path::Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn gunzip(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(data).read_to_end(&mut out).unwrap();
    out
}

// ---- the page ----------------------------------------------------------------------------------------------------

#[test]
fn test_the_page_and_its_assets_come_from_eve_web_dir() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let web = make_web(&tmp.path().join("elsewhere"));
        let opts = RigOpts { environ: env(&[("EVE_PROFILE", "lite"), ("EVE_WEB_DIR", web.to_str().unwrap())]), ..RigOpts::default() };
        let r = Rig::with(&tmp.path().join("rig"), opts).await;
        r.ctx.update(|p| p.web_dir = None); // EVE_WEB_DIR decides, as in the daemon
        let page = r.mac("GET", "/", (), Req::new()).await;
        assert!(page.status == 200 && page.body == INDEX);
        assert_eq!(page.header("content-type"), "text/html; charset=utf-8");
        assert_eq!(page.header("cache-control"), "no-store");
        let script = r.mac("GET", "/assets/eve-abc123.js", (), Req::new()).await;
        assert!(script.status == 200 && script.body == script_js());
        assert_eq!(script.header("content-type"), "text/javascript; charset=utf-8");
        assert_eq!(script.header("cache-control"), "public, max-age=31536000, immutable");
        assert!(!script.headers.contains_key("content-encoding")); // not asked for gzip
        let style = r.mac("GET", "/assets/eve-abc123.css", (), Req::new()).await;
        assert_eq!(style.header("content-type"), "text/css; charset=utf-8");
        // gzip when the browser takes it, the same bytes once undone
        let squeezed =
            r.mac("GET", "/assets/eve-abc123.js", (), Req::new().header("Accept-Encoding", "gzip, deflate, br")).await;
        assert!(squeezed.header("content-encoding") == "gzip" && squeezed.header("vary") == "Accept-Encoding");
        assert!(gunzip(&squeezed.body) == script_js() && squeezed.body.len() < script_js().len());
        let refused = r.mac("GET", "/assets/eve-abc123.js", (), Req::new().header("Accept-Encoding", "gzip;q=0")).await;
        assert!(!refused.headers.contains_key("content-encoding") && refused.body == script_js());
        // HEAD: the head alone
        let head = r.mac("HEAD", "/", (), Req::new()).await;
        assert!(head.status == 200 && head.body.is_empty() && head.header("content-length") == INDEX.len().to_string());
        // a changed file is served changed (the cache follows the file)
        let mut v2 = INDEX.to_vec();
        v2.extend_from_slice(b"<!-- v2 -->");
        std::fs::write(web.join("index.html"), &v2).unwrap();
        assert!(r.mac("GET", "/", (), Req::new()).await.body.ends_with(b"<!-- v2 -->"));
        r.stop().await;
    });
}

#[test]
fn test_only_files_inside_the_web_folder_are_served() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let r = Rig::start(tmp.path()).await;
        let web = r.ctx.web_root();
        std::fs::write(tmp.path().join("secret.txt"), "not for the page").unwrap();
        std::os::unix::fs::symlink(tmp.path().join("secret.txt"), web.join("assets").join("eve-link.js")).unwrap();
        std::fs::create_dir(web.join("assets").join("deeper")).unwrap();
        std::fs::write(web.join("assets").join("deeper").join("x.js"), "x").unwrap();
        for path in [
            "/assets/eve-link.js", "/assets/missing.js", "/assets/deeper/x.js", "/assets/../index.html",
            "/assets/%2e%2e/secret.txt", "/assets/.hidden", "/assets/", "/../secret.txt", "/secret.txt", "/web.json",
            "/env",
        ] {
            let reply = r.mac("GET", path, (), Req::new()).await;
            assert!(matches!(reply.status, 400 | 404), "{path} {}", reply.status);
            assert!(!contains(&reply.body, b"not for the page"));
        }
        assert_eq!(r.mac("GET", "/assets/eve-abc123.js", (), Req::new()).await.status, 200); // positive control
        r.stop().await;
    });
}

#[test]
fn test_no_built_page_is_a_plain_503_page_not_a_crash() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let empty = tmp.path().join("empty");
        std::fs::create_dir(&empty).unwrap();
        let r = Rig::with(&tmp.path().join("rig"), RigOpts { web: Some(empty), ..RigOpts::default() }).await;
        let reply = r.mac("GET", "/", (), Req::new()).await;
        assert!(reply.status == 503 && reply.header("content-type") == "text/html; charset=utf-8");
        assert!(contains(&reply.body, html_escape(NOT_BUILT).as_bytes()) && !contains(&reply.body, b"<script"));
        assert_eq!(r.mac("GET", "/assets/eve-abc123.js", (), Req::new()).await.status, 404);
        assert_eq!(r.mac("GET", HEALTH_PATH, (), Req::new()).await.status, 200); // the door itself is up
        r.stop().await;
    });
}

#[test]
fn test_healthz_says_the_state_with_no_cookie() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let r = Rig::start(tmp.path()).await;
        assert_eq!(r.mac("GET", HEALTH_PATH, (), Req::new()).await.json(), json!({"ok": true, "state": "READY"}));
        r.set_status(json!({"state": "NEEDS_OPENROUTER"}));
        assert_eq!(r.mac("GET", HEALTH_PATH, (), Req::new()).await.json(), json!({"ok": true, "state": "NEEDS_OPENROUTER"}));
        r.set_status(json!({"state": "sk-like lower-case words"})); // not a state word: not passed on
        assert_eq!(r.mac("GET", HEALTH_PATH, (), Req::new()).await.json()["state"], "NEEDS_OPENROUTER");
        r.stop().await;
    });
}

// ---- the Mac's sign-in -------------------------------------------------------------------------------------------

#[test]
fn test_the_macs_browser_signs_in_once_with_its_code() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let r = Rig::start(tmp.path()).await;
        let code = r.mac_login.issue();
        let reply = r.mac("POST", MAC_LOGIN_PATH, json!({"code": code}), Req::new().header("User-Agent", MAC_SAFARI)).await;
        assert!(reply.status == 200 && reply.json() == json!({"ok": true}));
        let token = cookie_of(&reply, eve_web_auth::COOKIE_MAC);
        assert_eq!(reply.cookies, vec![format!("eve_mac={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age=34560000")]);
        assert!(r.mac_login.used(Some(&code))); // `web open` deletes its file now
        let again = r.mac("POST", MAC_LOGIN_PATH, json!({"code": code}), Req::new()).await;
        assert!(again.status == 403 && again.json() == json!({"ok": false, "error": LOGIN_SPENT}));
        assert!(again.cookies.is_empty());
        let wrong = r.mac("POST", MAC_LOGIN_PATH, json!({"code": "A".repeat(22)}), Req::new()).await;
        assert_eq!(wrong.status, 403);
        // the cookie opens the Mac's routes
        let state = r.mac("GET", ONBOARD_STATE_PATH, (), Req::new().cookie(format!("eve_mac={token}"))).await;
        assert_eq!(state.status, 200);
        // opening Eve again with a live cookie keeps this browser's one device
        let reopened = r
            .mac("POST", MAC_LOGIN_PATH, json!({"code": r.mac_login.issue()}), Req::new().cookie(format!("eve_mac={token}")))
            .await;
        assert!(reopened.status == 200 && cookie_of(&reopened, eve_web_auth::COOKIE_MAC) == token);
        let devices = r.devices.list();
        assert!(devices.len() == 1 && devices[0].label == "This Mac · Safari");
        assert_eq!(devices[0].host, "localhost");
        r.stop().await;
    });
}

#[test]
fn test_the_setup_state_reads_the_key_the_skip_and_the_credit() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let r = Rig::with(tmp.path(), RigOpts { environ: env(&[("EVE_PROFILE", "lite")]), ..RigOpts::default() }).await;
        let mac = format!("eve_mac={}", r.login().await);
        let state = r.mac("GET", ONBOARD_STATE_PATH, (), Req::new().cookie(&mac)).await;
        assert_eq!(
            state.json(),
            json!({"openrouter": false, "openrouter_rejected": false, "credit": null, "composio": false,
                   "composio_skipped": false, "restarting": false, "error": ""})
        );
        assert_eq!(r.state.credit_calls.load(Ordering::SeqCst), 0); // no key: nothing to ask OpenRouter
        assert_eq!(cookie_of(&state, eve_web_auth::COOKIE_MAC), mac.split_once('=').unwrap().1); // set again
        r.ctx.parts().environ.set("OPENROUTER_API_KEY", CANARY_KEY);
        let state = r.mac("GET", ONBOARD_STATE_PATH, (), Req::new().cookie(&mac)).await;
        assert_eq!(state.json()["openrouter"], true);
        assert_eq!(state.json()["credit"], json!({"ok": true, "usd": 4.25, "funded": true}));
        r.mac("GET", ONBOARD_STATE_PATH, (), Req::new().cookie(&mac)).await;
        assert_eq!(r.state.credit_calls.load(Ordering::SeqCst), 1); // read once per CREDIT_TTL_SECS
        let skipped = r.mac("POST", COMPOSIO_SKIP_PATH, json!({}), Req::new().cookie(&mac)).await;
        assert!(skipped.status == 200 && skipped.json() == json!({"ok": true}));
        assert_eq!(r.mac("GET", ONBOARD_STATE_PATH, (), Req::new().cookie(&mac)).await.json()["composio_skipped"], true);
        assert_eq!(read_json(&tmp.path().join("web.json"))["composio_skipped"], true);
        assert_eq!(mode(&tmp.path().join("web.json")), 0o600);
        r.stop().await;
        let again = Rig::start(tmp.path()).await; // remembered across a restart
        let mac = format!("eve_mac={}", again.login().await);
        assert_eq!(again.mac("GET", ONBOARD_STATE_PATH, (), Req::new().cookie(&mac)).await.json()["composio_skipped"], true);
        again.stop().await;
    });
}

/// onboarding.credit answers {"rejected": True} on a 401: both the setup state and the status carry a top-level
/// openrouter_rejected so the page can offer a new sign-in, and the key stays "present".
#[test]
fn test_a_turned_down_key_shows_as_rejected_on_state_and_status() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let opts = RigOpts { environ: env(&[("EVE_PROFILE", "lite"), ("OPENROUTER_API_KEY", CANARY_KEY)]), ..RigOpts::default() };
        let r = Rig::with(tmp.path(), opts).await;
        *r.state.credit_answer.lock() = json!({"ok": false, "usd": null, "funded": null, "error": "HTTP 401", "rejected": true});
        let mac = format!("eve_mac={}", r.login().await);
        let state = r.mac("GET", ONBOARD_STATE_PATH, (), Req::new().cookie(&mac)).await.json();
        assert!(state["openrouter"] == true && state["openrouter_rejected"] == true);
        let status = r.mac("GET", STATUS_PATH, (), Req::new().cookie(&mac)).await.json();
        assert!(status["openrouter"] == true && status["openrouter_rejected"] == true);
        assert_eq!(status["credit"]["rejected"], true); // the flag rides the credit view too
        // a 500 is a passing error, not a rejection: the key is not turned down
        r.ctx.shared.forget_credit();
        *r.state.credit_answer.lock() = json!({"ok": false, "usd": null, "funded": null, "error": "HTTP 500"});
        let again = r.mac("GET", STATUS_PATH, (), Req::new().cookie(&mac)).await.json();
        assert_eq!(again["openrouter_rejected"], false);
        r.stop().await;
    });
}

#[test]
fn test_a_slow_credit_read_never_holds_the_page_up() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let opts = RigOpts { environ: env(&[("EVE_PROFILE", "lite"), ("OPENROUTER_API_KEY", CANARY_KEY)]), ..RigOpts::default() };
        let r = Rig::with(tmp.path(), opts).await;
        let (gate, open) = tokio::sync::watch::channel(false);
        r.ctx.update(|p| {
            p.limits.credit_wait_secs = 0.05;
            p.credit = Arc::new(move |_key: String| {
                let mut open = open.clone();
                async move {
                    let _ = open.wait_for(|o| *o).await;
                    Ok(json!({"ok": true, "usd": 1.5, "funded": true}))
                }
                .boxed()
            });
        });
        let mac = format!("eve_mac={}", r.login().await);
        let first = r.mac("GET", ONBOARD_STATE_PATH, (), Req::new().cookie(&mac)).await;
        assert!(first.status == 200 && first.json()["credit"].is_null()); // not known yet, and not waited for
        gate.send(true).unwrap();
        until(|| r.ctx.shared.credit().is_some()).await; // the read lands for the next page
        let later = r.mac("GET", ONBOARD_STATE_PATH, (), Req::new().cookie(&mac)).await;
        assert_eq!(later.json()["credit"], json!({"ok": true, "usd": 1.5, "funded": true}));
        r.stop().await;
    });
}

// ---- adding a phone, the devices, the LAN switch ---------------------------------------------------------------

#[test]
fn test_add_a_phone_opens_the_lan_door_and_draws_two_codes() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let r = Rig::start(tmp.path()).await;
        assert!(!r.local.lan_on() && r.state.alerts.load(Ordering::SeqCst) == 0);
        let opened = r.open_pairing(&r.login().await).await;
        let lan = r.ctx.shared.lan().unwrap();
        assert!(lan.running() && r.local.lan_on());
        assert_eq!(r.state.alerts.load(Ordering::SeqCst), 1); // macOS's Local Network alert, on purpose
        let code = opened["code"].as_str().unwrap();
        assert!(code.len() == 6 && code.bytes().all(|b| b.is_ascii_digit()));
        let left = opened["expires_in"].as_i64().unwrap();
        assert!((290..=300).contains(&left));
        let labels: Vec<&str> = opened["links"].as_array().unwrap().iter().map(|l| l["label"].as_str().unwrap()).collect();
        assert_eq!(labels, ["iPhone or iPad", "Android or other"]);
        let apple = opened["links"][0]["url"].as_str().unwrap();
        let other = opened["links"][1]["url"].as_str().unwrap();
        assert!(apple.starts_with(&format!("https://test-mac.local:{}/#p=", lan.port())));
        assert!(other.starts_with(&format!("https://{LAN_IP}:{}/#p=", lan.port())));
        let secret = secret_of(&opened, 0);
        assert!(other.ends_with(&secret) && secret.len() >= 16);
        for link in opened["links"].as_array().unwrap() {
            let qr = link["qr"].as_str().unwrap();
            assert!(qr.starts_with("data:image/svg+xml") && qr.contains("%3Csvg"));
        }
        assert!(opened["url_apple"] == apple && opened["qr_other"].as_str().unwrap().starts_with("data:image/svg+xml"));
        // the link holds exactly label, url and qr, in that order
        let keys: Vec<&String> = opened["links"][0].as_object().unwrap().keys().collect();
        assert_eq!(keys, ["label", "url", "qr"]);
        assert!(r.pairing.is_open());
        // the certificate names this Mac, and web.json remembers the door
        let names = r.tls.inner.names().unwrap();
        for name in ["test-mac.local", LAN_IP, "localhost"] {
            assert!(names.contains(name), "{name}");
        }
        let saved = read_json(&tmp.path().join("web.json"));
        assert!(saved["lan"] == true && saved["lan_port"] == lan.port() && saved["local_port"] == r.port);
        r.stop().await;
    });
}

#[test]
fn test_no_address_for_a_phone_says_so_and_opens_no_window() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let r = Rig::start(tmp.path()).await;
        r.ctx.update(|p| {
            p.hostname = Arc::new(|| Ok(String::new()));
            p.addresses = Arc::new(|| Ok(Vec::new()));
        });
        let mac = format!("eve_mac={}", r.login().await);
        let reply = r.mac("POST", PAIR_OPEN_PATH, json!({}), Req::new().cookie(mac)).await;
        assert!(reply.status == 503 && reply.json()["error"] == NO_ADDRESS);
        assert!(!r.pairing.is_open());
        r.stop().await;
    });
}

#[test]
fn test_the_device_list_marks_this_browser_and_a_revoke_closes_the_peer() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let r = Rig::start(tmp.path()).await;
        let mac_token = r.login().await;
        let mac = format!("eve_mac={mac_token}");
        let phone = r.pair(Some(&mac_token)).await;
        // the phone signals a WebRTC peer through the LAN door
        let rtc = r.lan("POST", RTC_PATH, json!({"sdp": "v=0\r\n"}), Req::new().cookie(format!("eve_dev={phone}"))).await;
        assert_eq!(rtc.status, 200);
        let listed = r.mac("GET", DEVICES_PATH, (), Req::new().cookie(&mac)).await.json()["devices"].clone();
        let by_label: std::collections::BTreeMap<String, Value> =
            listed.as_array().unwrap().iter().map(|d| (d["label"].as_str().unwrap().to_string(), d.clone())).collect();
        assert_eq!(by_label.keys().collect::<Vec<_>>(), ["This Mac · Safari", "iPhone · Safari"]);
        let (this_mac, iphone) = (&by_label["This Mac · Safari"], &by_label["iPhone · Safari"]);
        assert!(this_mac["current"] == true && iphone["current"] == false);
        let keys: std::collections::BTreeSet<&str> = iphone.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(iphone["host"], "localhost");
        assert_eq!(keys, ["created", "current", "host", "id", "label", "last_seen"].into_iter().collect());
        let text = listed.to_string();
        assert!(!text.contains(&phone) && !text.contains(&mac_token)); // hashes stay on disk
        let phone_id = iphone["id"].as_str().unwrap().to_string();
        let gone = r.mac("POST", REVOKE_PATH, json!({"id": phone_id}), Req::new().cookie(&mac)).await;
        assert!(gone.status == 200 && gone.json() == json!({"ok": true, "revoked": [phone_id]}));
        assert_eq!(*r.rtc.closed.lock(), vec![phone_id.clone()]); // its live peer closed
        assert_eq!(r.lan("GET", STATUS_PATH, (), Req::new().cookie(format!("eve_dev={phone}"))).await.status, 401);
        let twice = r.mac("POST", REVOKE_PATH, json!({"id": phone_id}), Req::new().cookie(&mac)).await;
        assert_eq!(twice.json(), json!({"ok": true, "revoked": []})); // twice is nothing more
        for bad in [json!({"id": "../x"}), json!({"id": 7}), json!({})] {
            assert_eq!(r.mac("POST", REVOKE_PATH, bad, Req::new().cookie(&mac)).await.status, 400);
        }
        let everything = r.mac("POST", REVOKE_PATH, json!({"id": "all"}), Req::new().cookie(&mac)).await;
        assert_eq!(everything.json()["revoked"], json!([this_mac["id"]]));
        assert_eq!(r.mac("GET", DEVICES_PATH, (), Req::new().cookie(&mac)).await.status, 401); // this browser too
        r.stop().await;
    });
}

#[test]
fn test_signing_out_revokes_this_browser_and_clears_its_cookie() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let r = Rig::start(tmp.path()).await;
        let token = r.login().await;
        let out = r.mac("POST", LOGOUT_PATH, json!({}), Req::new().cookie(format!("eve_mac={token}"))).await;
        assert!(out.status == 200 && out.json() == json!({"ok": true}));
        assert_eq!(out.cookies, ["eve_mac=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0"]);
        assert!(r.devices.list().is_empty());
        let refused = r.mac("GET", STATUS_PATH, (), Req::new().cookie(format!("eve_mac={token}"))).await;
        assert!(refused.status == 401 && refused.cookies == ["eve_mac=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0"]);
        r.stop().await;
    });
}

#[test]
fn test_the_lan_switch_turns_the_door_off_and_on_and_is_remembered() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let r = Rig::start(tmp.path()).await;
        let mac_token = r.login().await;
        let mac = format!("eve_mac={mac_token}");
        let phone = r.pair(Some(&mac_token)).await;
        r.lan("POST", RTC_PATH, json!({"sdp": "v=0\r\n"}), Req::new().cookie(format!("eve_dev={phone}"))).await;
        r.mac("POST", RTC_PATH, json!({"sdp": "v=0\r\n"}), Req::new().cookie(&mac)).await; // the Mac's own voice peer
        let lan_port = r.lan_port();
        r.pairing.open();
        let off = r.mac("POST", LAN_PATH, json!({"on": false}), Req::new().cookie(&mac)).await;
        assert!(off.status == 200 && off.json() == json!({"ok": true, "lan": false}));
        assert!(!r.pairing.is_open()); // a window closes with the door
        let phone_id = r.devices.list().into_iter().find(|d| d.label.starts_with("iPhone")).unwrap().id;
        assert_eq!(*r.rtc.closed.lock(), vec![phone_id]); // the LAN's peers, not the Mac's own
        assert!(tokio::net::TcpStream::connect(("127.0.0.1", lan_port)).await.is_err());
        assert_eq!(read_json(&tmp.path().join("web.json"))["lan"], false);
        let status = r.mac("GET", STATUS_PATH, (), Req::new().cookie(&mac)).await;
        assert_eq!(status.json()["lan"], false);
        r.ctx.update(|p| p.lan_ports = vec![lan_port, 0]); // as the daemon's range holds it
        let on = r.mac("POST", LAN_PATH, json!({"on": true}), Req::new().cookie(&mac)).await;
        assert!(on.json() == json!({"ok": true, "lan": true}) && r.lan_port() == lan_port); // the same address
        assert_eq!(r.lan("GET", STATUS_PATH, (), Req::new().cookie(format!("eve_dev={phone}"))).await.status, 200);
        for bad in [json!({"on": "yes"}), json!({"on": 1}), json!({})] {
            assert_eq!(r.mac("POST", LAN_PATH, bad, Req::new().cookie(&mac)).await.status, 400);
        }
        r.stop().await;
        // web.json says on: it opens at start
        let again = Rig::with(tmp.path(), RigOpts { lan_ports: vec![lan_port, 0], ..RigOpts::default() }).await;
        assert!(again.local.lan_on() && again.lan_port() == lan_port);
        again.stop().await;
    });
}

// ---- the chat, signaling, apps, status ------------------------------------------------------------------------

#[test]
fn test_the_macs_page_chats_with_the_unchanged_linkchat_over_loopback() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let inlet = FakeInlet::new(Some(store_at(tmp.path())), Some(two_lines()));
        let link = link_door(tmp.path(), inlet.clone());
        let r = Rig::with(&tmp.path().join("rig"), RigOpts { link: Some(link), ..RigOpts::default() }).await;
        let mac = format!("eve_mac={}", r.login().await);
        let turn = r
            .mac("POST", "/api/web/link/turn", json!({"text": "  hello eve  ", "client_id": "web-turn-1"}), Req::new().cookie(&mac))
            .await;
        assert!(turn.status == 200 && turn.header("content-type") == "text/event-stream");
        let (contents, done, _) = events(&turn.body);
        assert!(contents == ["First line.", "\n\nSecond line."] && done);
        assert_eq!(inlet.heard(), vec![("hello eve".to_string(), false, Some("web-turn-1".to_string()))]);
        assert_eq!(turn.header("referrer-policy"), "no-referrer"); // this door's headers, added
        assert_eq!(turn.header("x-content-type-options"), "nosniff");
        let history = r.mac("GET", "/api/web/link/messages", (), Req::new().cookie(&mac)).await;
        assert!(history.status == 200 && history.json()["messages"].is_array());
        let up = r
            .mac(
                "POST",
                "/api/web/link/upload",
                b"some notes\n".to_vec(),
                Req::new().ctype("application/octet-stream").header("X-Filename", "notes.txt").cookie(&mac),
            )
            .await;
        assert!(up.status == 200 && up.json()["ok"] == true && up.json()["kind"] == "text", "{}", up.text());
        let id = up.json()["id"].as_str().unwrap().to_string();
        let kept = r.mac("GET", &format!("/api/web/link/file/{id}"), (), Req::new().cookie(&mac)).await;
        assert!(kept.status == 200 && kept.body == b"some notes\n");
        assert_eq!(kept.header("x-content-type-options"), "nosniff"); // LinkChat's own, not doubled
        assert_eq!(kept.header("referrer-policy"), "no-referrer");
        r.stop().await;
    });
}

#[test]
fn test_the_chat_before_setup_is_a_503_with_a_line() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let r = Rig::start(tmp.path()).await;
        let mac = format!("eve_mac={}", r.login().await);
        let reply = r.mac("GET", "/api/web/link/messages", (), Req::new().cookie(mac)).await;
        assert!(reply.status == 503 && reply.json() == json!({"ok": false, "error": NOT_READY}));
        r.stop().await;
    });
}

#[test]
fn test_rtc_signaling_passes_the_device_and_its_address() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let r = Rig::start(tmp.path()).await;
        let mac = format!("eve_mac={}", r.login().await);
        let me = r.devices.list()[0].id.clone();
        let answer = r.mac("POST", RTC_PATH, json!({"sdp": "v=0\r\noffer"}), Req::new().cookie(&mac)).await;
        assert!(answer.status == 200);
        assert_eq!(answer.json(), json!({"sdp": "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\n", "voice": true}));
        assert_eq!(*r.rtc.offers.lock(), vec![(me, "v=0\r\noffer".to_string(), "127.0.0.1".to_string())]);
        let bad = r.mac("POST", RTC_PATH, json!({"sdp": 42}), Req::new().cookie(&mac)).await;
        assert!(bad.status == 400 && bad.json()["error"].as_str().unwrap().contains("SDP"));
        assert!(r.state.probes.lock().is_empty()); // loopback: nothing to probe
        r.ctx.update(|p| p.rtc = None);
        let off = r.mac("POST", RTC_PATH, json!({"sdp": "v=0\r\n"}), Req::new().cookie(&mac)).await;
        assert!(off.status == 503 && off.json()["error"] == RTC_OFF);
        r.stop().await;
    });
}

#[test]
fn test_apps_list_the_connected_apps_and_hand_out_composio_links_only() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let bridge = FakeBridge::new(&[("gmail", true)]);
        let r = Rig::with(tmp.path(), RigOpts { apps: Some(bridge.clone()), ..RigOpts::default() }).await;
        let mac = format!("eve_mac={}", r.login().await);
        let c = || Req::new().cookie(&mac);
        assert_eq!(r.mac("GET", APPS_PATH, (), c()).await.json(), json!({"composio": false, "apps": []}));
        std::fs::write(tmp.path().join("composio-connect.json"), "{}").unwrap(); // signed in (onboarding writes it)
        let listed = r.mac("GET", APPS_PATH, (), c()).await.json();
        assert_eq!(listed["composio"], true);
        let apps = listed["apps"].as_array().unwrap();
        let slugs: Vec<&str> = apps.iter().map(|a| a["toolkit"].as_str().unwrap()).collect();
        assert_eq!(slugs, eve_composio_connect::app_slugs());
        assert_eq!(apps.iter().find(|a| a["toolkit"] == "gmail").unwrap()["connected"], true);
        assert_eq!(apps.iter().find(|a| a["toolkit"] == "googlecalendar").unwrap()["name"], "Google Calendar");
        let link = r.mac("POST", "/api/web/apps/slack/connect", json!({}), c()).await;
        assert!(link.status == 200 && link.json() == json!({"ok": true, "url": "https://connect.composio.dev/link/ln_test"}));
        let already = r.mac("POST", "/api/web/apps/gmail/connect", json!({}), c()).await;
        assert!(already.status == 409 && already.json()["error"] == "Gmail is already connected.");
        assert_eq!(r.mac("POST", "/api/web/apps/tavily/connect", json!({}), c()).await.status, 404); // web search
        assert_eq!(r.mac("POST", "/api/web/apps/Gmail%20x/connect", json!({}), c()).await.status, 404);
        *bridge.link.lock() = Some("https://composio.dev.example.com/link".to_string()); // a look-alike host
        let foreign = r.mac("POST", "/api/web/apps/notion/connect", json!({}), c()).await;
        assert!(foreign.status == 502 && foreign.json().get("url").is_none());
        bridge.needs_sign_in.store(true, Ordering::SeqCst); // the sign-in lapsed for good
        assert_eq!(r.mac("GET", APPS_PATH, (), c()).await.json()["composio"], false);
        assert_eq!(r.mac("POST", "/api/web/apps/slack/connect", json!({}), c()).await.status, 409);
        r.stop().await;
    });
}

#[test]
fn test_status_is_booleans_words_and_the_credit() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let opts = RigOpts { environ: env(&[("EVE_PROFILE", "lite"), ("OPENROUTER_API_KEY", CANARY_KEY)]), ..RigOpts::default() };
        let r = Rig::with(tmp.path(), opts).await;
        let mac = format!("eve_mac={}", r.login().await);
        let status = r.mac("GET", STATUS_PATH, (), Req::new().cookie(&mac)).await.json();
        assert_eq!(
            status,
            json!({"state": "READY", "openrouter": true, "openrouter_rejected": false, "composio": false,
                   "voice": true, "lan": false, "local_network": "unknown", "restarting": false,
                   "credit": {"ok": true, "usd": 4.25, "funded": true}})
        );
        r.rtc.voice.store(false, Ordering::SeqCst);
        r.set_status(json!({"state": "READY", "voice": true})); // the daemon's word wins
        assert_eq!(r.mac("GET", STATUS_PATH, (), Req::new().cookie(&mac)).await.json()["voice"], true);
        r.set_status(json!({"state": "READY"}));
        assert_eq!(r.mac("GET", STATUS_PATH, (), Req::new().cookie(&mac)).await.json()["voice"], false);
        r.stop().await;
    });
}

// ---- the listener ------------------------------------------------------------------------------------------------

#[test]
fn test_the_mac_door_listens_on_loopback_only() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let mut r = Rig::start(tmp.path()).await;
        r.local.stop().await;
        r.ctx.update(|p| p.local_hosts = LOCAL_HOSTS.iter().map(|s| s.to_string()).collect()); // 127.0.0.1 and ::1
        let port = r.local.start().await.unwrap();
        r.port = port;
        let bound = r.local.bound();
        assert!(bound.iter().all(|(a, _)| a == "127.0.0.1" || a == "::1"));
        assert!(bound.contains(&("127.0.0.1".to_string(), port)));
        assert!(bound.iter().all(|(_, p)| *p == port));
        assert_eq!(r.mac("GET", HEALTH_PATH, (), Req::new()).await.status, 200);
        if bound.iter().any(|(a, _)| a == "::1") {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut conn = tokio::net::TcpStream::connect(("::1", port)).await.unwrap();
            conn.write_all(format!("GET /healthz HTTP/1.1\r\nHost: [::1]:{port}\r\n\r\n").as_bytes()).await.unwrap();
            let mut got = Vec::new();
            tokio::time::timeout(Duration::from_secs(10), conn.read_to_end(&mut got)).await.unwrap().unwrap();
            assert!(got.starts_with(b"HTTP/1.1 200"));
        }
        r.stop().await;
    });
}

#[test]
fn test_the_port_web_json_names_comes_first_and_a_taken_one_is_passed_over() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let taken = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let busy = taken.local_addr().unwrap().port();
        let mut r = Rig::start(tmp.path()).await;
        r.local.stop().await;
        r.ctx.update(|p| p.local_ports = vec![busy, 0]);
        let port = r.local.start().await.unwrap();
        r.port = port;
        assert!(port != busy && r.mac("GET", HEALTH_PATH, (), Req::new()).await.status == 200);
        assert_eq!(read_json(&tmp.path().join("web.json"))["local_port"], port);
        r.local.stop().await;
        r.ctx.update(|p| p.local_ports = vec![0, port]); // the remembered port first
        r.ctx.shared.saved.set_held(Change::LocalPort(port));
        assert_eq!(r.local.start().await.unwrap(), port);
        r.local.stop().await;
        r.ctx.update(|p| p.local_ports = vec![busy]);
        let err = r.local.start().await.unwrap_err();
        assert!(matches!(err, StartError::PortsTaken(_)), "{err:?}");
        r.ctx.update(|p| p.local_ports = vec![0]);
        r.port = r.local.start().await.unwrap(); // for the rig's own stop
        r.stop().await;
        drop(taken);
    });
}

#[test]
fn test_the_doors_are_eve_lites_only() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let err = Rig::try_with(tmp.path(), RigOpts { environ: env(&[]), ..RigOpts::default() }).await.err().unwrap();
        assert!(matches!(&err, StartError::Runtime(line) if line.contains("EVE_PROFILE=lite")), "{err:?}");
        let full = RigOpts { environ: env(&[("EVE_PROFILE", "full")]), ..RigOpts::default() };
        let err = Rig::try_with(&tmp.path().join("full"), full).await.err().unwrap();
        assert!(matches!(err, StartError::Runtime(_)), "{err:?}");
    });
}

#[test]
fn test_stopping_ends_an_open_turn_with_done_first() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let inlet = FakeInlet::new(Some(store_at(tmp.path())), Some(forever()));
        let link = link_door(tmp.path(), inlet.clone());
        let rig = Rig::with(&tmp.path().join("rig"), RigOpts { link: Some(link), ..RigOpts::default() }).await;
        let mac = format!("eve_mac={}", rig.login().await);
        let port = rig.port;
        let pending = tokio::spawn(async move {
            http(port, "POST", "/api/web/link/turn", json!({"text": "hi", "client_id": "t-stop-1"}), Req::new().cookie(mac)).await
        });
        for _ in 0..200 {
            if !inlet.heard().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        rig.stop().await;
        let turn = tokio::time::timeout(Duration::from_secs(10), pending).await.unwrap().unwrap();
        let (contents, done, _) = events(&turn.body);
        assert!(contents == ["Working on it."] && done); // [DONE], not a cut stream
    });
}

#[test]
fn test_a_door_never_logs_a_path_an_address_or_a_cookie() {
    let tmp = tempfile::tempdir().unwrap();
    let ((token, phone), text) = with_logs(|| {
        run(async {
            let r = Rig::start(tmp.path()).await;
            let token = r.login().await;
            let phone = r.pair(None).await;
            for path in ["/onboard/state", "/no/such/path-7f3c", "/api/web/status"] {
                r.mac("GET", path, (), Req::new().cookie(format!("eve_mac={token}"))).await;
            }
            r.lan("GET", STATUS_PATH, (), Req::new().cookie(format!("eve_dev={phone}"))).await;
            r.mac("GET", "/", (), Req::new().host("evil.example:80")).await;
            r.stop().await;
            (token, phone)
        })
    });
    for leak in [
        token.as_str(), phone.as_str(), "/no/such/path-7f3c", "/onboard/state", "evil.example", &HOSTNAME.to_lowercase(),
        LAN_IP, "127.0.0.1",
    ] {
        assert!(!text.contains(leak), "{leak}");
    }
    assert!(text.contains("lite web: the Mac's page is up on port"), "{text}"); // positive control: it does log
}

#[test]
fn test_pairing_and_devices_files_are_owner_only() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let r = Rig::start(tmp.path()).await;
        r.pair(None).await;
        for name in ["web.json", "web-devices.json"] {
            assert_eq!(mode(&tmp.path().join(name)), 0o600, "{name}");
        }
        assert_eq!(mode(&tmp.path().join("web-tls")), 0o700);
        assert!(std::fs::read_dir(tmp.path().join("web-tls")).unwrap().next().is_some());
        r.stop().await;
    });
}

#[test]
fn test_a_callback_url_names_this_door_and_its_port() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let r = Rig::start(tmp.path()).await;
        let mac = format!("eve_mac={}", r.login().await);
        let start = r.mac("GET", OPENROUTER_START_PATH, (), Req::new().cookie(&mac)).await;
        assert_eq!(start.status, 302);
        let asked = query_of(start.header("location"));
        assert_eq!(asked["callback_url"], format!("http://localhost:{}{OPENROUTER_CALLBACK_PATH}", r.port));
        assert!(asked["code_challenge_method"] == "S256" && !asked["state"].is_empty());
        let composio = r.mac("GET", COMPOSIO_START_PATH, (), Req::new().cookie(&mac)).await;
        assert_eq!(composio.status, 302);
        let asked = query_of(composio.header("location"));
        assert_eq!(asked["redirect_uri"], format!("http://localhost:{}{COMPOSIO_CALLBACK_PATH}", r.port));
        assert_eq!(asked["resource"], eve_brain::onboarding::CONNECT_RESOURCE);
        r.stop().await;
    });
}

#[test]
fn test_a_restart_that_stops_the_doors_itself_is_not_cut_short() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let mut r = Rig::start(tmp.path()).await;
        let stopped = Arc::new(parking_lot::Mutex::new(Vec::<bool>::new()));
        let (local, seen) = (r.local.clone(), stopped.clone());
        r.ctx.update(|p| {
            p.restart = Some(Arc::new(move || {
                let (local, seen) = (local.clone(), seen.clone());
                async move {
                    local.stop().await; // as lite.py may: stop, then exit 75
                    tokio::time::sleep(Duration::from_millis(10)).await; // more to close after the doors
                    seen.lock().push(true);
                    Ok(())
                }
                .boxed()
            }));
        });
        let mac = format!("eve_mac={}", r.login().await);
        let start = r.mac("GET", OPENROUTER_START_PATH, (), Req::new().cookie(&mac)).await;
        let state = query_of(start.header("location"))["state"].clone();
        let back = r.mac("GET", &format!("{OPENROUTER_CALLBACK_PATH}?code=c&state={state}"), (), Req::new()).await;
        assert_eq!(back.status, 303);
        for _ in 0..500 {
            if !stopped.lock().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(*stopped.lock() == [true] && !r.local.running()); // the restart ran to its end
        r.port = r.local.start().await.unwrap(); // for the rig's own stop
        r.stop().await;
    });
}

#[test]
fn test_a_composio_sign_in_that_cannot_start_says_why_on_a_page_and_in_setup() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let r = Rig::with(tmp.path(), RigOpts { register_status: 400, ..RigOpts::default() }).await;
        let mac = format!("eve_mac={}", r.login().await);
        let reply = r.mac("GET", COMPOSIO_START_PATH, (), Req::new().cookie(&mac)).await;
        let line = eve_brain::onboarding::COMPOSIO_NOT_REGISTERED.replace("{status}", "400");
        assert!(reply.status == 502 && reply.header("content-type") == "text/html; charset=utf-8");
        assert!(contains(&reply.body, html_escape(&line).as_bytes()) && !reply.headers.contains_key("location"));
        assert_eq!(r.provider_calls(), [eve_brain::onboarding::CONNECT_REGISTER_URL]);
        assert_eq!(r.mac("GET", ONBOARD_STATE_PATH, (), Req::new().cookie(&mac)).await.json()["error"], line);
        r.ctx.update(|p| p.openrouter = None);
        let gone = r.mac("GET", OPENROUTER_START_PATH, (), Req::new().cookie(&mac)).await;
        assert!(gone.status == 503 && contains(&gone.body, html_escape(NO_SIGNIN).as_bytes()));
        r.stop().await;
    });
}

#[test]
fn test_turning_the_lan_door_off_says_so_when_web_json_cannot_keep_it() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let r = Rig::start(tmp.path()).await;
        let token = r.login().await;
        let mac = format!("eve_mac={token}");
        r.open_pairing(&token).await;
        r.ctx.shared.saved.set_writer(Arc::new(|_path, _data| Err(std::io::Error::from_raw_os_error(28))));
        let off = r.mac("POST", LAN_PATH, json!({"on": false}), Req::new().cookie(&mac)).await;
        assert!(off.status == 500 && off.json()["error"] == NOT_SAVED);
        assert!(!r.local.lan_on()); // off for now, as asked
        assert!(r.ctx.shared.saved.lan()); // and the page was told it will not last
        assert_eq!(read_json(&tmp.path().join("web.json"))["lan"], true);
        let skip = r.mac("POST", COMPOSIO_SKIP_PATH, json!({}), Req::new().cookie(&mac)).await;
        assert!(skip.status == 500 && !r.ctx.shared.saved.composio_skipped());
        r.stop().await;
    });
}

// ---- the helpers (beyond the Python file: the pure functions it exercises through the doors) -----------------

#[test]
fn helpers_match_python() {
    assert_eq!(ports(&[48780, 48782, 48780, 48783], Some(48783)), vec![48783, 48780, 48782]);
    assert_eq!(ports(&[1, 2], Some(9)), vec![1, 2]);
    assert_eq!(plain("  two\n words\t", "fb"), "two words");
    assert_eq!(plain("", "fb"), "fb");
    assert_eq!(plain(&format!("bad key {CANARY_KEY}"), "fb"), "fb");
    assert_eq!(plain(&"a ".repeat(400), "fb").chars().count(), 300);
    assert_eq!(device_label(Some(IPHONE), "lan"), "iPhone · Safari");
    assert_eq!(device_label(Some(MAC_SAFARI), "local"), "This Mac · Safari");
    assert_eq!(device_label(None, "lan"), "A device");
    assert_eq!(host_name("kitchen-mac.local:48781"), "kitchen-mac.local");
    assert_eq!(host_name("[::1]:80"), "[::1]");
    assert_eq!(host_name("[::1]"), "[::1]");
    assert_eq!(host_name("localhost:"), "localhost:");
    let q = query("code=a%2Bb+c&state=s&code=second&error");
    assert_eq!(q["code"], "a+b c");
    assert_eq!(q["state"], "s");
    assert_eq!(q["error"], "");
    assert!(query(&"a=1&".repeat(32)).is_empty()); // 33 fields: more than 32
    assert_eq!(query(&"a=1&".repeat(31)).len(), 1);
    assert_eq!(content_type("assets/x.JS"), "text/javascript; charset=utf-8");
    assert_eq!(content_type("assets/.hidden"), "application/octet-stream");
    assert_eq!(content_type("index.html"), HTML_TYPE);
    assert!(accepts_gzip("gzip, deflate"));
    assert!(accepts_gzip("br;q=1, *;q=0.5"));
    assert!(!accepts_gzip("gzip;q=0"));
    assert!(!accepts_gzip("gzip;q=x"));
    assert!(!accepts_gzip("br"));
    assert_eq!(credit_view(&json!([1])), None);
    assert_eq!(
        credit_view(&json!({"ok": true, "usd": 2, "funded": "yes", "error": "HTTP 401", "rejected": true, "key": "k"})),
        Some(json!({"ok": true, "usd": 2.0, "funded": null, "error": "HTTP 401", "rejected": true}))
    );
    assert_eq!(credit_view(&json!({"ok": 1, "usd": true, "error": "Bad—reason"})), Some(json!({"ok": false, "usd": null, "funded": null})));
    assert!(key_rejected(Some(&json!({"rejected": true}))) && !key_rejected(Some(&json!({"rejected": 1}))) && !key_rejected(None));
    let (r, arg) = route_for("/api/web/link/file/0123456789abcdef0123456789abcdef").unwrap();
    assert!(r.cookie() && r.methods() == ["GET"] && arg == "file/0123456789abcdef0123456789abcdef");
    assert!(route_for("/api/web/link/file/ZZ").is_none());
    assert_eq!(route_for("/assets/eve-1.js").unwrap().1, "eve-1.js");
    assert_eq!(route_for("/api/web/apps/gmail/connect").unwrap().1, "gmail");
    assert!(route_for("/assets/.x").is_none());
    let page = String::from_utf8(page_html("a<b", "\"it's\"")).unwrap();
    assert!(page.contains("<title>a&lt;b</title>") && page.contains("<p>&quot;it&#x27;s&quot;</p>"));
}

/// Beyond the Python file: a read that landed after its page stopped waiting is done, so the next page past the
/// TTL starts a new one (`task.done()`), never the old answer again.
#[test]
fn a_credit_read_that_landed_late_is_not_reused() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let opts = RigOpts { environ: env(&[("EVE_PROFILE", "lite"), ("OPENROUTER_API_KEY", CANARY_KEY)]), ..RigOpts::default() };
        let r = Rig::with(tmp.path(), opts).await;
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = calls.clone();
        r.ctx.update(|p| {
            p.limits.credit_wait_secs = 0.01;
            p.credit = Arc::new(move |_key: String| {
                let n = counted.fetch_add(1, Ordering::SeqCst) + 1;
                async move {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    Ok(json!({"ok": true, "usd": n as f64, "funded": true}))
                }
                .boxed()
            });
        });
        let mac = format!("eve_mac={}", r.login().await);
        assert!(r.mac("GET", ONBOARD_STATE_PATH, (), Req::new().cookie(&mac)).await.json()["credit"].is_null());
        until(|| r.ctx.shared.credit().is_some()).await;
        r.ctx.shared.forget_credit(); // as if CREDIT_TTL_SECS went by
        r.ctx.update(|p| p.limits.credit_wait_secs = 5.0);
        let fresh = r.mac("GET", ONBOARD_STATE_PATH, (), Req::new().cookie(&mac)).await.json();
        assert_eq!(fresh["credit"]["usd"], 2.0);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        r.stop().await;
    });
}
