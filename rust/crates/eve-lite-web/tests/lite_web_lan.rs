//! test_lite_web_lan.py: LanServer, the door phones and laptops on the same Wi-Fi use.
//!
//! Real TLS with the rig's own local CA (lite_tls.LocalTls) on loopback ports, and clients that trust that CA, as a
//! phone that installed it does. The rig is the local tests' Rig: the Mac's page adds the phone, the phone pairs here.

mod common;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use common::*;
use eve_lite_web::*;
use rustls::pki_types::ServerName;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const WINDOW_GONE: &str = NO_WINDOW;

/// The first bytes a TLS client sends: a real ClientHello, from rustls.
fn client_hello() -> Vec<u8> {
    let name = ServerName::try_from("localhost").unwrap();
    let mut conn = rustls::ClientConnection::new(client_untrusting(), name).unwrap();
    let mut hello = Vec::new();
    while conn.wants_write() {
        conn.write_tls(&mut hello).unwrap();
    }
    assert_eq!(hello[0], 0x16); // a handshake record
    hello
}

// ---- pairing ----------------------------------------------------------------------------------------------------

#[test]
fn test_a_phone_pairs_with_the_qr_codes_secret_and_gets_a_secure_cookie() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let r = Rig::start(tmp.path()).await;
        let opened = r.open_pairing(&r.login().await).await;
        let secret = secret_of(&opened, 0);
        let paired = r.lan("POST", PAIR_PATH, json!({"secret": secret}), Req::new().header("User-Agent", IPHONE)).await;
        assert!(paired.status == 200 && paired.json() == json!({"ok": true}));
        let token = cookie_of(&paired, eve_web_auth::COOKIE_DEV);
        assert_eq!(paired.cookies, [format!("eve_dev={token}; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=34560000")]);
        assert!(!r.pairing.is_open()); // one device per window
        let status = r.lan("GET", STATUS_PATH, (), Req::new().cookie(format!("eve_dev={token}"))).await;
        assert!(status.status == 200 && status.json()["state"] == "READY");
        assert_eq!(cookie_of(&status, eve_web_auth::COOKIE_DEV), token); // set again: 400 days from this visit
        let phone = r.devices.list().into_iter().find(|d| d.label == "iPhone · Safari").unwrap();
        assert_eq!(phone.host, "localhost");
        until(|| r.ctx.shared.local_network() == "ok").await;
        assert_eq!(r.state.probes.lock()[0].ip().to_string(), "127.0.0.1"); // the Local Network probe, at pairing
        assert!(matches!(status.json()["local_network"].as_str(), Some("unknown" | "ok")));
        let again = r.lan("GET", STATUS_PATH, (), Req::new().cookie(format!("eve_dev={token}"))).await;
        assert_eq!(again.json()["local_network"], "ok");
        let reused = r.lan("POST", PAIR_PATH, json!({"secret": secret}), Req::new()).await;
        assert!(reused.status == 403 && reused.json()["error"] == WINDOW_GONE && reused.cookies.is_empty());
        r.stop().await;
    });
}

#[test]
fn test_a_laptop_pairs_with_the_six_digit_code_and_five_wrong_tries_close_the_window() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let r = Rig::start(tmp.path()).await;
        let mac = r.login().await;
        let code = r.open_pairing(&mac).await["code"].as_str().unwrap().to_string();
        let wrong = format!("{:06}", (code.parse::<u32>().unwrap() + 1) % 1_000_000);
        for _ in 0..eve_web_auth::PAIR_TRIES - 1 {
            let reply = r.lan("POST", PAIR_PATH, json!({"code": wrong}), Req::new()).await;
            assert!(reply.status == 403 && reply.json()["error"] == WRONG_CODE && reply.cookies.is_empty());
        }
        let last = r.lan("POST", PAIR_PATH, json!({"code": wrong}), Req::new()).await;
        assert!(last.status == 429 && last.json()["error"] == TOO_MANY);
        let closed = r.lan("POST", PAIR_PATH, json!({"code": code}), Req::new()).await; // the right code, too late
        assert!(closed.status == 403 && closed.json()["error"] == WINDOW_GONE && closed.cookies.is_empty());
        let code = r.open_pairing(&mac).await["code"].as_str().unwrap().to_string();
        let both = r.lan("POST", PAIR_PATH, json!({"code": code, "secret": "x".repeat(22)}), Req::new()).await;
        assert_eq!(both.status, 403); // both at once is a wrong try
        let typed = r.lan("POST", PAIR_PATH, json!({"code": format!("{} {}", &code[..3], &code[3..])}), Req::new()).await;
        assert!(typed.status == 200 && !cookie_of(&typed, eve_web_auth::COOKIE_DEV).is_empty());
        r.stop().await;
    });
}

#[test]
fn test_a_new_window_replaces_the_old_one() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let r = Rig::start(tmp.path()).await;
        let mac = r.login().await;
        let first = secret_of(&r.open_pairing(&mac).await, 0);
        let second = secret_of(&r.open_pairing(&mac).await, 0);
        assert_ne!(first, second);
        assert_eq!(r.lan("POST", PAIR_PATH, json!({"secret": first}), Req::new()).await.status, 403);
        assert_eq!(r.lan("POST", PAIR_PATH, json!({"secret": second}), Req::new()).await.status, 200);
        r.stop().await;
    });
}

#[test]
fn test_no_window_means_no_pairing() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let r = Rig::start(tmp.path()).await;
        r.open_pairing(&r.login().await).await;
        r.pairing.close();
        let reply = r.lan("POST", PAIR_PATH, json!({"code": "123456"}), Req::new()).await;
        assert!(reply.status == 403 && reply.json()["error"] == WINDOW_GONE && reply.cookies.is_empty());
        assert!(device_labels(&r.devices).iter().all(|l| l.starts_with("This Mac")));
        r.stop().await;
    });
}

// ---- what the LAN door has, and what it does not --------------------------------------------------------------

fn mac_only() -> Vec<(&'static str, String)> {
    vec![
        ("GET", ONBOARD_STATE_PATH.into()),
        ("GET", OPENROUTER_START_PATH.into()),
        ("GET", format!("{OPENROUTER_CALLBACK_PATH}?code=c&state=s")),
        ("GET", COMPOSIO_START_PATH.into()),
        ("GET", format!("{COMPOSIO_CALLBACK_PATH}?code=c&state=s")),
        ("POST", COMPOSIO_SKIP_PATH.into()),
        ("POST", MAC_LOGIN_PATH.into()),
        ("POST", PAIR_OPEN_PATH.into()),
        ("GET", DEVICES_PATH.into()),
        ("POST", REVOKE_PATH.into()),
        ("POST", LAN_PATH.into()),
        ("GET", "/onboard/anything".into()),
        ("GET", "/api/mac/anything".into()),
    ]
}

#[test]
fn test_the_lan_door_has_no_setup_and_none_of_the_macs_routes() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let r = Rig::start(tmp.path()).await;
        let phone = r.pair(None).await;
        for (method, path) in mac_only() {
            let body: Body = if method == "POST" {
                json!({"code": r.mac_login.issue(), "id": "all", "on": false}).into()
            } else {
                Body::None
            };
            let reply = r.lan(method, &path, body, Req::new().cookie(format!("eve_dev={phone}"))).await;
            assert_eq!(reply.status, 404, "{method} {path}");
            assert!(reply.json() == json!({"ok": false, "error": NOT_FOUND}) && reply.cookies.is_empty());
        }
        assert!(r.provider_calls().is_empty() && r.restarts() == 0); // no sign-in started or finished
        assert!(r.local.lan_on() && r.devices.list().len() == 2); // nothing turned off or revoked
        assert_eq!(r.lan("GET", STATUS_PATH, (), Req::new().cookie(format!("eve_dev={phone}"))).await.status, 200);
        r.stop().await;
    });
}

#[test]
fn test_the_ca_downloads_carry_no_key_and_are_the_lan_doors_only() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let r = Rig::start(tmp.path()).await;
        r.open_pairing(&r.login().await).await;
        let pem = r.lan("GET", CA_PATH, (), Req::new()).await; // before pairing: no cookie
        assert!(pem.status == 200 && pem.header("content-type") == "application/x-x509-ca-cert");
        assert_eq!(pem.header("content-disposition"), "attachment; filename=\"eve-lite-ca.crt\"");
        assert!(pem.body == r.tls.inner.ca_pem().unwrap() && pem.body.starts_with(b"-----BEGIN CERTIFICATE-----"));
        let profile = r.lan("GET", MOBILECONFIG_PATH, (), Req::new()).await;
        assert!(profile.status == 200 && profile.header("content-type") == "application/x-apple-aspen-config");
        let parsed: plist::Value = plist::from_bytes(&profile.body).unwrap();
        let dict = parsed.as_dictionary().unwrap();
        assert_eq!(dict["PayloadType"].as_string(), Some("Configuration"));
        let kinds: Vec<&str> = dict["PayloadContent"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p.as_dictionary().unwrap()["PayloadType"].as_string().unwrap())
            .collect();
        assert_eq!(kinds, ["com.apple.security.root"]);
        for body in [&pem.body, &profile.body] {
            assert!(!contains(body, b"PRIVATE KEY"));
        }
        assert!(contains(&std::fs::read(tmp.path().join("web-tls").join("ca.key")).unwrap(), b"PRIVATE KEY")); // control
        for path in [CA_PATH, MOBILECONFIG_PATH] {
            assert_eq!(r.mac("GET", path, (), Req::new()).await.status, 404); // the Mac's page has no use for them
        }
        r.stop().await;
    });
}

#[test]
fn test_the_lan_hosts_are_the_certificates_names_and_the_macs_addresses() {
    let cases = [
        ("test-mac.local", true),
        ("TEST-MAC.LOCAL", true),
        (LAN_IP, true),
        ("localhost", true),
        ("127.0.0.1", true),
        ("evil.example", false),
        ("test-mac.local.evil.example", false),
        ("192.168.50.8", false),
        ("[::1]", false),
    ];
    for (host, ok) in cases {
        let tmp = tempfile::tempdir().unwrap();
        run(async {
            let r = Rig::start(tmp.path()).await;
            r.open_pairing(&r.login().await).await;
            let port = r.lan_port();
            let reply = r.lan("GET", HEALTH_PATH, (), Req::new().host(format!("{host}:{port}"))).await;
            assert_eq!(reply.status, if ok { 200 } else { 421 }, "{host}");
            if ok {
                for wrong in [host.to_string(), format!("{host}:{}", port + 1)] {
                    // no port, or another
                    assert_eq!(r.lan("GET", HEALTH_PATH, (), Req::new().host(wrong)).await.status, 421);
                }
            }
            r.stop().await;
        });
    }
}

#[test]
fn test_a_new_address_reissues_the_certificate_at_add_a_phone() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let r = Rig::start(tmp.path()).await;
        let mac = r.login().await;
        r.open_pairing(&mac).await;
        let port = r.lan_port();
        let leaf = || std::fs::read(&r.tls.inner.leaf_cert_path).unwrap();
        let first = leaf();
        assert_eq!(r.lan("GET", HEALTH_PATH, (), Req::new().host(format!("192.168.50.8:{port}"))).await.status, 421);
        r.ctx.update(|p| p.addresses = Arc::new(|| Ok(vec!["192.168.50.8".to_string()]))); // DHCP gave a new address
        let opened = r.open_pairing(&mac).await;
        assert!(opened["links"][1]["url"].as_str().unwrap().starts_with(&format!("https://192.168.50.8:{port}/#p=")));
        assert_ne!(leaf(), first); // reissued with it
        let names = r.tls.inner.names().unwrap();
        for name in ["192.168.50.8", LAN_IP, "test-mac.local"] {
            assert!(names.contains(name), "{name}"); // the old one kept too
        }
        r.ctx.shared.lan().unwrap().expire_names(); // past NAMES_TTL_SECS
        assert_eq!(r.lan("GET", HEALTH_PATH, (), Req::new().host(format!("192.168.50.8:{port}"))).await.status, 200);
        let unchanged = leaf();
        r.open_pairing(&mac).await; // nothing new: no reissue
        assert_eq!(leaf(), unchanged);
        r.stop().await;
    });
}

// ---- TLS, and the peer filter before it ------------------------------------------------------------------------

#[test]
fn test_the_lan_door_speaks_tls_1_2_or_later_with_the_local_cas_certificate() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let r = Rig::start(tmp.path()).await;
        r.open_pairing(&r.login().await).await;
        let port = r.lan_port();
        let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let name = ServerName::try_from(format!("{}.local", HOSTNAME.to_lowercase())).unwrap();
        let mut conn = tokio_rustls::TlsConnector::from(r.trust()).connect(name, tcp).await.unwrap();
        let version = conn.get_ref().1.protocol_version().unwrap();
        assert!(matches!(version, rustls::ProtocolVersion::TLSv1_2 | rustls::ProtocolVersion::TLSv1_3));
        conn.write_all(format!("GET /healthz HTTP/1.1\r\nHost: test-mac.local:{port}\r\n\r\n").as_bytes()).await.unwrap();
        let mut got = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(10), conn.read_to_end(&mut got)).await.unwrap();
        assert!(got.starts_with(b"HTTP/1.1 200"));
        // a browser that has not installed the CA sees an unknown certificate (the click-through warning)
        let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let refused = tokio_rustls::TlsConnector::from(client_untrusting())
            .connect(ServerName::try_from("localhost").unwrap(), tcp)
            .await
            .expect_err("an unknown certificate");
        assert!(refused.to_string().to_lowercase().contains("certificate"), "{refused}");
        // plain HTTP to the LAN door gets no HTTP back
        let plain = answer_to(port, format!("GET /healthz HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n").as_bytes()).await;
        assert!(!contains(&plain, b"HTTP/1.1"));
        r.stop().await;
    });
}

#[test]
fn test_the_peer_filter_admits_home_networks_and_refuses_the_rest() {
    let sock = |s: &str| eve_lan_net::Peer::Socket(s.parse::<SocketAddr>().unwrap());
    let text = |s: &str| eve_lan_net::Peer::from(s);
    for peer in [
        text("127.0.0.1"),
        sock("192.168.1.20:50000"),
        text("10.1.2.3"),
        text("172.16.5.4"),
        text("169.254.3.3"),
        text("fe80::1%en0"),
        sock("[fd12:3456::1]:50000"),
        text("::ffff:192.168.1.9"),
    ] {
        assert!(eve_lan_net::peer_allowed(peer.clone()), "{peer:?}");
    }
    for peer in [
        text("8.8.8.8"),
        sock("1.1.1.1:443"),
        text("100.64.0.1"),
        text("2001:4860:4860::8888"),
        text("198.51.100.7"),
        text(""),
        eve_lan_net::Peer::None,
        text("not an address"),
    ] {
        assert!(!eve_lan_net::peer_allowed(peer.clone()), "{peer:?}");
    }
}

#[test]
fn test_a_peer_from_outside_is_dropped_before_tls() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let allow = Arc::new(AtomicBool::new(true));
        let gate = allow.clone();
        let opts = RigOpts {
            peer_allowed: Some(Arc::new(move |_peer: &SocketAddr| Ok(gate.load(Ordering::SeqCst)))),
            ..RigOpts::default()
        };
        let r = Rig::with(tmp.path(), opts).await;
        r.open_pairing(&r.login().await).await;
        let lan = r.ctx.shared.lan().unwrap();
        let port = lan.port();
        let before = r.tls.contexts.load(Ordering::SeqCst);
        allow.store(false, Ordering::SeqCst);
        assert!(answer_to(port, &client_hello()).await.is_empty()); // closed, not a byte back
        assert!(answer_to(port, b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n").await.is_empty());
        assert_eq!(lan.dropped(), 2);
        assert_eq!(r.tls.contexts.load(Ordering::SeqCst), before); // no TLS work at all for them
        assert!(try_http(port, "GET", HEALTH_PATH, (), Req::new().tls(r.trust())).await.is_err());
        allow.store(true, Ordering::SeqCst); // positive control: a home peer
        assert_eq!(http(port, "GET", HEALTH_PATH, (), Req::new().tls(r.trust())).await.status, 200);
        assert!(r.tls.contexts.load(Ordering::SeqCst) == before + 1 && lan.dropped() == 3);
        r.stop().await;
    });
}

#[test]
fn test_rtc_through_the_lan_door_passes_the_phones_address_and_probes_it() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let r = Rig::start(tmp.path()).await;
        let phone = r.pair(None).await;
        until(|| !r.state.probes.lock().is_empty()).await; // the pairing's own probe
        r.state.probes.lock().clear();
        let answer = r.lan("POST", RTC_PATH, json!({"sdp": "v=0\r\noffer"}), Req::new().cookie(format!("eve_dev={phone}"))).await;
        assert!(answer.status == 200 && answer.json()["voice"] == true);
        let (device_id, sdp, peer) = r.rtc.offers.lock().last().unwrap().clone();
        assert!(sdp == "v=0\r\noffer" && peer == "127.0.0.1");
        assert_eq!(r.ctx.shared.rtc_doors()[&device_id], "lan");
        until(|| !r.state.probes.lock().is_empty()).await;
        assert_eq!(r.state.probes.lock()[0].ip().to_string(), "127.0.0.1");
        r.stop().await;
    });
}

#[test]
fn test_the_chat_over_https_reaches_the_same_linkchat() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let inlet = FakeInlet::new(Some(store_at(tmp.path())), Some(two_lines()));
        let link = link_door(tmp.path(), inlet.clone());
        let r = Rig::with(&tmp.path().join("rig"), RigOpts { link: Some(link), ..RigOpts::default() }).await;
        let phone = r.pair(None).await;
        let turn = r
            .lan(
                "POST",
                "/api/web/link/turn",
                json!({"text": "hi from the phone", "client_id": "lan-turn-1"}),
                Req::new().cookie(format!("eve_dev={phone}")),
            )
            .await;
        assert_eq!(turn.status, 200);
        let (contents, done, _) = events(&turn.body);
        assert!(contents == ["First line.", "\n\nSecond line."] && done);
        assert_eq!(inlet.heard(), [("hi from the phone".to_string(), false, Some("lan-turn-1".to_string()))]);
        r.stop().await;
    });
}

#[test]
fn test_a_lan_door_started_on_its_own_still_checks_everything() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let r = Rig::start(tmp.path()).await;
        let lan = LanServer::new(Arc::clone(&r.ctx));
        let port = lan.start().await.unwrap();
        assert!(r.ctx.shared.lan().unwrap().same(&lan) && r.local.lan_on());
        assert_eq!(http(port, "GET", HEALTH_PATH, (), Req::new().tls(r.trust())).await.status, 200);
        assert_eq!(http(port, "GET", STATUS_PATH, (), Req::new().tls(r.trust())).await.status, 401);
        assert_eq!(http(port, "GET", HEALTH_PATH, (), Req::new().tls(r.trust()).host("evil.example:1")).await.status, 421);
        let saved: Value = serde_json::from_slice(&std::fs::read(tmp.path().join("web.json")).unwrap()).unwrap();
        assert_eq!(saved["lan_port"], port);
        lan.stop().await;
        assert!(!r.local.lan_on());
        r.stop().await;
    });
}

#[test]
fn test_an_unusable_certificate_keeps_phones_out_and_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let ((), text) = with_logs(|| {
        run(async {
            let r = Rig::start(tmp.path()).await;
            r.open_pairing(&r.login().await).await;
            let port = r.lan_port();
            *r.tls.mode.lock() = TlsMode::Broken;
            assert!(answer_to(port, &client_hello()).await.is_empty());
            r.stop().await;
        })
    });
    assert!(text.contains("the LAN certificate could not be loaded (OSError)"), "{text}");
}
