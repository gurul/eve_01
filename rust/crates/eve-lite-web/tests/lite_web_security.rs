//! test_lite_web_security.py: the Host allowlist, Origin on anything but a GET, the content type, the device cookie
//! and its exact flags, the OAuth callbacks' single-use state, that no JSON answer can carry a key, the security
//! headers, what reaches LinkChat, and requests that are not plain HTTP/1.1.
//!
//! Every refusal here has its positive control beside it: the same request with the one thing fixed goes through.

mod common;

use std::sync::atomic::Ordering;
use std::sync::{Arc, LazyLock};

use common::*;
use eve_brain::onboarding;
use eve_lite_web::*;
use futures::FutureExt;
use regex::Regex;
use serde_json::{json, Value};

/// What a key, a token or a code looks like in a JSON answer: the canaries, OpenRouter's and OpenAI's key shapes, a
/// JWT, and any long opaque run.
static KEY_SHAPED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"CANARY-NOT-A-KEY|sk-or-v1-|sk-[A-Za-z0-9]{16,}|eyJ[A-Za-z0-9_-]{10,}|[A-Za-z0-9_+/=-]{32,}").unwrap()
});

fn key_shaped(text: &str) -> Vec<String> {
    KEY_SHAPED.find_iter(text).map(|m| m.as_str().to_string()).collect()
}

fn mode(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

// ---- Host --------------------------------------------------------------------------------------------------------

#[test]
fn test_a_foreign_host_is_421_on_both_doors() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let r = Rig::start(tmp.path()).await;
        let token = r.login().await;
        let mac = format!("eve_mac={token}");
        let p = r.port;
        for host in [
            "evil.example".to_string(),
            format!("evil.example:{p}"),
            format!("localhost:{}", p + 1),
            format!("localhost.:{p}"),
            format!("127.0.0.2:{p}"),
            format!("[::2]:{p}"),
            format!("localhost:{p}@evil.example"),
            "localhost".to_string(),
            String::new(),
        ] {
            for path in ["/", STATUS_PATH] {
                // the page and a cookie route alike
                let reply = r.mac("GET", path, (), Req::new().host(&host).cookie(&mac)).await;
                assert_eq!(reply.status, 421, "{host} {path}");
                assert!(reply.json() == json!({"ok": false, "error": WRONG_HOST}) && reply.cookies.is_empty());
            }
        }
        // a DNS-rebinding POST: the right Origin for its own name, the wrong Host for eve
        let rebound = r.mac("POST", PAIR_OPEN_PATH, json!({}), Req::new().host(format!("evil.example:{p}")).cookie(&mac)).await;
        assert!(rebound.status == 421 && !r.pairing.is_open());
        for host in [format!("localhost:{p}"), format!("127.0.0.1:{p}"), format!("[::1]:{p}"), format!("LOCALHOST:{p}")] {
            assert_eq!(r.mac("GET", STATUS_PATH, (), Req::new().host(&host).cookie(&mac)).await.status, 200, "{host}");
        }
        r.open_pairing(&token).await;
        let lan = r.lan_port();
        assert_eq!(r.lan("GET", "/", (), Req::new().host(format!("evil.example:{lan}"))).await.status, 421);
        assert_eq!(r.lan("GET", "/", (), Req::new().host(format!("localhost:{p}"))).await.status, 421); // the Mac page's port
        assert_eq!(r.lan("GET", "/", (), Req::new().host(format!("localhost:{lan}"))).await.status, 200);
        r.stop().await;
    });
}

// ---- Origin, the body's type ---------------------------------------------------------------------------------------

/// Every POST the Mac's page has: (path, body, content type).
fn post_routes(r: &Rig) -> Vec<(&'static str, Body, Option<&'static str>)> {
    vec![
        (MAC_LOGIN_PATH, json!({"code": r.mac_login.issue()}).into(), None),
        (COMPOSIO_SKIP_PATH, json!({}).into(), None),
        (PAIR_OPEN_PATH, json!({}).into(), None),
        (REVOKE_PATH, json!({"id": "all"}).into(), None),
        (LAN_PATH, json!({"on": true}).into(), None),
        (RTC_PATH, json!({"sdp": "v=0\r\n"}).into(), None),
        (LOGOUT_PATH, json!({}).into(), None),
        ("/api/web/apps/gmail/connect", json!({}).into(), None),
        ("/api/web/link/turn", json!({"text": "hi", "client_id": "csrf-1"}).into(), None),
        ("/api/web/link/upload", b"notes\n".to_vec().into(), Some("application/octet-stream")),
    ]
}

#[test]
fn test_a_post_needs_the_pages_own_origin_and_a_get_does_not() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let inlet = FakeInlet::new(Some(store_at(tmp.path())), Some(two_lines()));
        let opts = RigOpts {
            link: Some(link_door(tmp.path(), inlet.clone())),
            apps: Some(FakeBridge::new(&[])),
            ..RigOpts::default()
        };
        let r = Rig::with(&tmp.path().join("rig"), opts).await;
        let token = r.login().await;
        let mac = format!("eve_mac={token}");
        let p = r.port;
        for (path, body, ctype) in post_routes(&r) {
            for origin in [
                None,
                Some("http://evil.example".to_string()),
                Some("null".to_string()),
                Some(format!("http://localhost:{}", p + 1)),
                Some(format!("https://localhost:{p}")),
                Some(format!("http://localhost:{p}.evil.example")),
                Some(String::new()),
            ] {
                let mut req = Req::new().origin(origin.as_deref()).cookie(&mac);
                if let Some(ct) = ctype {
                    req = req.ctype(ct);
                }
                let reply = r.mac("POST", path, body.clone(), req).await;
                assert_eq!(reply.status, 403, "{path} {origin:?}");
                assert_eq!(reply.json(), json!({"ok": false, "error": WRONG_ORIGIN}));
            }
        }
        // nothing happened: no window, no device revoked, the door still off, nothing heard, still signed in
        assert!(!r.pairing.is_open() && r.devices.list().len() == 1);
        assert!(!r.local.lan_on() && inlet.heard().is_empty() && r.rtc.offers.lock().is_empty());
        assert!(!r.ctx.shared.saved.composio_skipped());
        // positive controls: a GET needs no Origin; the same POST from the page's own origin goes through
        assert_eq!(r.mac("GET", STATUS_PATH, (), Req::new().cookie(&mac).origin(None)).await.status, 200);
        assert_eq!(r.mac("GET", STATUS_PATH, (), Req::new().cookie(&mac).origin(Some("http://evil.example"))).await.status, 200);
        assert_eq!(r.mac("POST", COMPOSIO_SKIP_PATH, json!({}), Req::new().cookie(&mac)).await.status, 200);
        let upper = format!("http://LOCALHOST:{p}");
        let rtc = r.mac("POST", RTC_PATH, json!({"sdp": "v=0\r\n"}), Req::new().cookie(&mac).origin(Some(&upper))).await;
        assert_eq!(rtc.status, 200);
        // the LAN door's own origin is https
        let phone = r.pair(Some(&token)).await;
        let lan = r.lan_port();
        for origin in [None, Some(format!("http://localhost:{lan}")), Some(format!("https://localhost:{p}")), Some("https://evil.example".to_string())] {
            let reply = r
                .lan("POST", RTC_PATH, json!({"sdp": "v=0\r\n"}), Req::new().cookie(format!("eve_dev={phone}")).origin(origin.as_deref()))
                .await;
            assert_eq!(reply.status, 403, "{origin:?}");
        }
        let ok = r.lan("POST", RTC_PATH, json!({"sdp": "v=0\r\n"}), Req::new().cookie(format!("eve_dev={phone}"))).await;
        assert_eq!(ok.status, 200);
        r.stop().await;
    });
}

#[test]
fn test_a_post_body_must_be_json_or_an_upload_of_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let inlet = FakeInlet::new(Some(store_at(tmp.path())), Some(two_lines()));
        let opts = RigOpts { link: Some(link_door(tmp.path(), inlet.clone())), ..RigOpts::default() };
        let r = Rig::with(&tmp.path().join("rig"), opts).await;
        let mac = format!("eve_mac={}", r.login().await);
        for ctype in ["text/plain", "application/x-www-form-urlencoded", "multipart/form-data; boundary=x", "", "application/jsonx", "application/octet-stream"] {
            let reply = r.mac("POST", COMPOSIO_SKIP_PATH, b"{}".to_vec(), Req::new().ctype(ctype).cookie(&mac)).await;
            assert_eq!(reply.status, 415, "{ctype}");
        }
        for ctype in ["application/json", "text/plain", ""] {
            let req = Req::new().ctype(ctype).cookie(&mac).header("X-Filename", "notes.txt");
            let reply = r.mac("POST", "/api/web/link/upload", b"notes\n".to_vec(), req).await;
            assert_eq!(reply.status, 415, "{ctype}");
        }
        assert!(!r.ctx.shared.saved.composio_skipped() && inlet.heard().is_empty());
        let chunked = r
            .mac("POST", COMPOSIO_SKIP_PATH, b"2\r\n{}\r\n0\r\n\r\n".to_vec(), Req::new().cookie(&mac).header("Transfer-Encoding", "chunked"))
            .await;
        assert!(matches!(chunked.status, 400 | 411));
        for (raw, status) in [(b"[1, 2]".to_vec(), 400), (b"{not json".to_vec(), 400), (vec![b'x'; MAX_JSON_BYTES + 1], 413)] {
            let reply = r.mac("POST", COMPOSIO_SKIP_PATH, raw.clone(), Req::new().cookie(&mac)).await;
            assert_eq!(reply.status, status, "{:?}", &raw[..10.min(raw.len())]);
        }
        assert!(!r.ctx.shared.saved.composio_skipped());
        // positive controls
        let ok = r.mac("POST", COMPOSIO_SKIP_PATH, b"{}".to_vec(), Req::new().ctype("application/json; charset=utf-8").cookie(&mac)).await;
        assert_eq!(ok.status, 200);
        let up = r
            .mac("POST", "/api/web/link/upload", b"notes\n".to_vec(), Req::new().ctype("application/octet-stream").cookie(&mac).header("X-Filename", "notes.txt"))
            .await;
        assert!(up.status == 200 && up.json()["ok"] == true);
        r.stop().await;
    });
}

#[test]
fn test_a_content_length_that_is_not_ascii_digits_is_400() {
    // "²".isdigit() is True and int() of it raises: the request got a 500 instead of a plain refusal
    for length in [&b"\xb2"[..], b"1\xb2", b"-1", b"+2"] {
        let tmp = tempfile::tempdir().unwrap();
        run(async {
            let r = Rig::start(tmp.path()).await;
            let mac = format!("eve_mac={}", r.login().await);
            let mut raw = format!(
                "POST {COMPOSIO_SKIP_PATH} HTTP/1.1\r\nHost: localhost:{p}\r\nOrigin: http://localhost:{p}\r\nCookie: {mac}\r\nContent-Type: application/json\r\n",
                p = r.port
            )
            .into_bytes();
            raw.extend_from_slice(b"Content-Length: ");
            raw.extend_from_slice(length);
            raw.extend_from_slice(b"\r\n\r\n{}");
            let reply = send_raw(r.port, &raw, None, 20.0).await.unwrap();
            assert!(reply.starts_with(b"HTTP/1.1 400 "), "{:?}", String::from_utf8_lossy(&reply[..80.min(reply.len())]));
            assert!(!r.ctx.shared.saved.composio_skipped());
            r.stop().await;
        });
    }
}

// ---- the cookie --------------------------------------------------------------------------------------------------

fn cookie_gets() -> Vec<String> {
    vec![STATUS_PATH.into(), APPS_PATH.into(), "/api/web/link/messages".into(), format!("/api/web/link/file/{}", "a".repeat(32))]
}
const MAC_COOKIE_GETS: [&str; 4] = [ONBOARD_STATE_PATH, DEVICES_PATH, OPENROUTER_START_PATH, COMPOSIO_START_PATH];
const MAC_COOKIE_POSTS: [&str; 4] = [COMPOSIO_SKIP_PATH, PAIR_OPEN_PATH, REVOKE_PATH, LAN_PATH];
const COOKIE_POSTS: [&str; 4] = [RTC_PATH, LOGOUT_PATH, "/api/web/apps/gmail/connect", "/api/web/link/turn"];

fn body_for(path: &str) -> Value {
    match path {
        REVOKE_PATH => json!({"id": "all"}),
        LAN_PATH => json!({"on": false}),
        RTC_PATH => json!({"sdp": "v=0\r\n"}),
        "/api/web/link/turn" => json!({"text": "hi", "client_id": "c-401"}),
        _ => json!({}),
    }
}

#[test]
fn test_a_missing_or_wrong_cookie_is_401_on_every_cookie_route() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let inlet = FakeInlet::new(Some(store_at(tmp.path())), Some(two_lines()));
        let opts = RigOpts {
            link: Some(link_door(tmp.path(), inlet.clone())),
            apps: Some(FakeBridge::new(&[])),
            ..RigOpts::default()
        };
        let r = Rig::with(&tmp.path().join("rig"), opts).await;
        let mac_token = r.login().await;
        let phone = r.pair(Some(&mac_token)).await;
        let wrong_mac = [
            None,
            Some(format!("eve_dev={phone}")),
            Some(format!("eve_mac={}", "x".repeat(43))),
            Some(format!("eve_mac={}", &mac_token[..mac_token.len() - 1])),
            Some(format!("other={mac_token}")),
        ];
        for cookie in &wrong_mac {
            let gets = cookie_gets().into_iter().chain(MAC_COOKIE_GETS.iter().map(|s| s.to_string()));
            for path in gets {
                let reply = r.mac("GET", &path, (), Req::new().maybe_cookie(cookie.as_deref())).await;
                assert_eq!(reply.status, 401, "{cookie:?} {path}");
                assert_eq!(reply.json(), json!({"ok": false, "error": SIGNED_OUT_MAC}));
            }
            for path in COOKIE_POSTS.iter().chain(MAC_COOKIE_POSTS.iter()) {
                let reply = r.mac("POST", path, body_for(path), Req::new().maybe_cookie(cookie.as_deref())).await;
                assert_eq!(reply.status, 401, "{cookie:?} {path}");
            }
        }
        let wrong_dev = [None, Some(format!("eve_mac={mac_token}")), Some(format!("eve_dev={}", &phone[..phone.len() - 1]))];
        for cookie in &wrong_dev {
            // the LAN door's own name only
            for path in cookie_gets() {
                let reply = r.lan("GET", &path, (), Req::new().maybe_cookie(cookie.as_deref())).await;
                assert!(reply.status == 401 && reply.json()["error"] == SIGNED_OUT_DEV, "{cookie:?} {path}");
            }
            for path in COOKIE_POSTS {
                assert_eq!(r.lan("POST", path, body_for(path), Req::new().maybe_cookie(cookie.as_deref())).await.status, 401);
            }
        }
        // nothing reached: no turn heard, no peer, no window, devices as they were
        assert!(inlet.heard().is_empty() && r.rtc.offers.lock().is_empty() && r.devices.list().len() == 2);
        assert!(r.local.lan_on());
        // positive controls: each door's own cookie
        assert_eq!(r.mac("GET", STATUS_PATH, (), Req::new().cookie(format!("eve_mac={mac_token}"))).await.status, 200);
        assert_eq!(r.lan("GET", STATUS_PATH, (), Req::new().cookie(format!("eve_dev={phone}"))).await.status, 200);
        let mixed = format!("theme=dark; eve_mac={mac_token}; x=1");
        assert_eq!(r.mac("GET", STATUS_PATH, (), Req::new().cookie(mixed)).await.status, 200);
        // a revoked device's cookie is refused and cleared
        r.devices.revoke("all").unwrap();
        let gone = r.lan("GET", STATUS_PATH, (), Req::new().cookie(format!("eve_dev={phone}"))).await;
        assert_eq!(gone.status, 401);
        assert_eq!(gone.cookies, ["eve_dev=; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=0"]);
        r.stop().await;
    });
}

/// A device paired on the LAN door gets an eve_dev token. Sent back as the Mac page's eve_mac on the loopback door
/// (open to every account on the Mac), it is refused on every Mac-only route: the device is bound to the door it
/// paired through (web_auth.Devices).
#[test]
fn test_a_phone_token_as_eve_mac_reaches_no_mac_only_route() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let r = Rig::with(tmp.path(), RigOpts { apps: Some(FakeBridge::new(&[])), ..RigOpts::default() }).await;
        let mac_token = r.login().await;
        let phone = r.pair(Some(&mac_token)).await; // a phone's eve_dev token
        let as_mac = format!("eve_mac={phone}");
        for path in MAC_COOKIE_GETS {
            let reply = r.mac("GET", path, (), Req::new().cookie(&as_mac)).await;
            assert_eq!(reply.status, 401, "{path}");
            assert_eq!(reply.json(), json!({"ok": false, "error": SIGNED_OUT_MAC}));
        }
        for path in MAC_COOKIE_POSTS {
            let body = if path == LAN_PATH { json!({"on": false}) } else { body_for(path) };
            assert_eq!(r.mac("POST", path, body, Req::new().cookie(&as_mac)).await.status, 401, "{path}");
        }
        // the sign-in redirects, which carry the owner's accounts to the provider, never start with this token
        for path in [OPENROUTER_START_PATH, COMPOSIO_START_PATH] {
            assert_eq!(r.mac("GET", path, (), Req::new().cookie(&as_mac)).await.status, 401, "{path}");
        }
        // nothing moved: the owner's devices are intact, the LAN door still on, no new window
        assert!(r.devices.list().len() == 2 && r.local.lan_on() && !r.pairing.is_open());
        // control: the phone's own token on its own door still works
        assert_eq!(r.lan("GET", STATUS_PATH, (), Req::new().cookie(format!("eve_dev={phone}"))).await.status, 200);
        r.stop().await;
    });
}

#[test]
fn test_cookie_flags_are_exact_per_door() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let r = Rig::start(tmp.path()).await;
        let reply = r.mac("POST", MAC_LOGIN_PATH, json!({"code": r.mac_login.issue()}), Req::new()).await;
        let mac = cookie_of(&reply, eve_web_auth::COOKIE_MAC);
        assert_eq!(reply.cookies, [format!("eve_mac={mac}; HttpOnly; SameSite=Strict; Path=/; Max-Age=34560000")]);
        assert!(!reply.cookies[0].contains("Secure")); // plain http on loopback
        let secret = secret_of(&r.open_pairing(&mac).await, 0);
        let paired = r.lan("POST", PAIR_PATH, json!({"secret": secret}), Req::new()).await;
        let dev = cookie_of(&paired, eve_web_auth::COOKIE_DEV);
        assert_eq!(paired.cookies, [format!("eve_dev={dev}; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=34560000")]);
        assert!(dev != mac && dev.len() >= 43); // 256-bit tokens, one per device
        let again = r.mac("GET", STATUS_PATH, (), Req::new().cookie(format!("eve_mac={mac}"))).await;
        assert_eq!(again.cookies, [format!("eve_mac={mac}; HttpOnly; SameSite=Strict; Path=/; Max-Age=34560000")]);
        let stored = std::fs::read_to_string(tmp.path().join("web-devices.json")).unwrap();
        assert!(!stored.contains(&mac) && !stored.contains(&dev)); // only their hashes are kept
        r.stop().await;
    });
}

// ---- the OAuth callbacks -----------------------------------------------------------------------------------------

#[test]
fn test_the_openrouter_callback_is_taken_with_state_alone_and_refused_when_reused() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let r = Rig::start(tmp.path()).await;
        let mac = format!("eve_mac={}", r.login().await);
        let start = r.mac("GET", OPENROUTER_START_PATH, (), Req::new().cookie(&mac)).await;
        assert!(start.status == 302 && start.header("location").starts_with(&format!("{}?", onboarding::OPENROUTER_AUTH_URL)));
        let state = query_of(start.header("location"))["state"].clone();
        let callback = format!("{OPENROUTER_CALLBACK_PATH}?code=or-code-1&state={state}");
        // OpenRouter's redirect back: no cookie (SameSite=Strict is not sent cross-site), no Origin
        let back = r.mac("GET", &callback, (), Req::new()).await;
        assert!(back.status == 303 && back.header("location") == "/", "{} {}", back.status, back.text());
        assert_eq!(r.provider_calls(), [onboarding::OPENROUTER_KEYS_URL]);
        let env_file = tmp.path().join("env");
        let parsed = eve_envfile::parse_env_file(&std::fs::read_to_string(&env_file).unwrap());
        assert_eq!(parsed["OPENROUTER_API_KEY"], CANARY_KEY);
        assert_eq!(mode(&env_file), 0o600);
        assert_eq!(r.ctx.parts().environ.get("OPENROUTER_API_KEY").as_deref(), Some(CANARY_KEY));
        until(|| r.restarts() == 1).await; // eve restarts into the key
        let now = r.mac("GET", ONBOARD_STATE_PATH, (), Req::new().cookie(&mac)).await.json();
        assert!(now["openrouter"] == true && now["restarting"] == true && now["error"] == "");
        // the same callback again: refused, with no second exchange
        let reused = r.mac("GET", &callback, (), Req::new()).await;
        assert!(reused.status == 400 && reused.header("content-type") == "text/html; charset=utf-8");
        assert!(contains(&reused.body, b"already used or is out of date"));
        assert_eq!(r.provider_calls(), [onboarding::OPENROUTER_KEYS_URL]);
        assert_eq!(r.mac("GET", ONBOARD_STATE_PATH, (), Req::new().cookie(&mac)).await.json()["error"], onboarding::STATE_UNKNOWN);
        // a state never issued, none at all, and a denial: each refused before anything leaves the Mac
        for q in ["code=c&state=never-issued-state", "code=c", ""] {
            let reply = r.mac("GET", &format!("{OPENROUTER_CALLBACK_PATH}?{q}"), (), Req::new()).await;
            assert_eq!(reply.status, 400, "{q}");
        }
        let fresh = r.mac("GET", OPENROUTER_START_PATH, (), Req::new().cookie(&mac)).await;
        let denied_state = query_of(fresh.header("location"))["state"].clone();
        let denied = r.mac("GET", &format!("{OPENROUTER_CALLBACK_PATH}?error=access_denied&state={denied_state}"), (), Req::new()).await;
        assert_eq!(denied.status, 400);
        assert!(r.provider_calls() == [onboarding::OPENROUTER_KEYS_URL] && r.restarts() == 1);
        // starting again clears the line the setup page shows
        r.mac("GET", OPENROUTER_START_PATH, (), Req::new().cookie(&mac)).await;
        assert_eq!(r.mac("GET", ONBOARD_STATE_PATH, (), Req::new().cookie(&mac)).await.json()["error"], "");
        r.stop().await;
    });
}

#[test]
fn test_the_composio_callback_is_taken_with_state_alone_and_refused_when_reused() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let r = Rig::start(tmp.path()).await;
        let mac = format!("eve_mac={}", r.login().await);
        let start = r.mac("GET", COMPOSIO_START_PATH, (), Req::new().cookie(&mac)).await;
        assert!(start.status == 302 && start.header("location").starts_with(onboarding::CONNECT_AUTHORIZE_URL));
        let state = query_of(start.header("location"))["state"].clone();
        let callback = format!("{COMPOSIO_CALLBACK_PATH}?code=cc-code-1&state={state}&iss=whatever");
        let back = r.mac("GET", &callback, (), Req::new()).await;
        assert!(back.status == 303 && back.header("location") == "/", "{} {}", back.status, back.text());
        let token_file = tmp.path().join("composio-connect.json");
        assert_eq!(mode(&token_file), 0o600);
        let saved: Value = serde_json::from_slice(&std::fs::read(&token_file).unwrap()).unwrap();
        assert_eq!(saved["access_token"], format!("{CANARY_TOKEN}-access"));
        until(|| r.restarts() == 1).await;
        assert_eq!(r.mac("GET", ONBOARD_STATE_PATH, (), Req::new().cookie(&mac)).await.json()["composio"], true);
        let calls = r.provider_calls();
        assert_eq!(calls, [onboarding::CONNECT_REGISTER_URL, onboarding::CONNECT_TOKEN_URL]);
        let reused = r.mac("GET", &callback, (), Req::new()).await;
        assert!(reused.status == 400 && r.provider_calls() == calls);
        let cancelled = r.mac("GET", &format!("{COMPOSIO_CALLBACK_PATH}?error=access_denied"), (), Req::new()).await;
        assert!(cancelled.status == 400 && r.provider_calls() == calls && r.restarts() == 1);
        r.stop().await;
    });
}

#[test]
fn test_a_restart_that_fails_lets_the_page_go_on() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let r = Rig::start(tmp.path()).await;
        r.ctx.update(|p| p.restart = Some(Arc::new(|| async { Err(anyhow::anyhow!("launchd said no")) }.boxed())));
        let mac = format!("eve_mac={}", r.login().await);
        let start = r.mac("GET", OPENROUTER_START_PATH, (), Req::new().cookie(&mac)).await;
        let state = query_of(start.header("location"))["state"].clone();
        let back = r.mac("GET", &format!("{OPENROUTER_CALLBACK_PATH}?code=c1&state={state}"), (), Req::new()).await;
        assert_eq!(back.status, 303);
        until(|| !r.ctx.shared.restarting()).await; // not stuck waiting for ever
        assert_eq!(r.mac("GET", ONBOARD_STATE_PATH, (), Req::new().cookie(&mac)).await.json()["openrouter"], true);
        r.stop().await;
    });
}

// ---- what the JSON answers may carry ------------------------------------------------------------------------------

#[test]
fn test_the_key_scanner_catches_a_planted_canary() {
    assert!(!key_shaped(&json!({"credit": {"error": CANARY_KEY}}).to_string()).is_empty());
    assert!(!key_shaped(&json!({"x": format!("sk-or-v1-{}", "0a".repeat(32))}).to_string()).is_empty());
    assert!(!key_shaped("eyJhbGciOiJIUzI1NiJ9.e30.x").is_empty());
    assert!(key_shaped(&json!({"state": "READY", "openrouter": true, "credit": {"ok": true, "usd": 4.25}}).to_string()).is_empty());
}

fn leaky_view(raw: &Value) -> Option<Value> {
    Some(raw.clone())
}

#[test]
fn test_no_json_answer_carries_a_key_shaped_string() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let opts = RigOpts {
            environ: env(&[("EVE_PROFILE", "lite"), ("OPENROUTER_API_KEY", CANARY_KEY)]),
            apps: Some(FakeBridge::new(&[("gmail", true)])),
            ..RigOpts::default()
        };
        let r = Rig::with(tmp.path(), opts).await;
        let planted = json!({"version": 1, "access_token": CANARY_TOKEN, "refresh_token": CANARY_TOKEN, "client_id": "c"});
        std::fs::write(tmp.path().join("composio-connect.json"), planted.to_string()).unwrap();
        // everything that feeds an answer tries to carry a canary
        r.set_status(json!({"state": CANARY_KEY, "openrouter": true, "voice": true, "key": CANARY_KEY,
                            "detail": {"token": CANARY_TOKEN}}));
        *r.state.credit_answer.lock() =
            json!({"ok": true, "usd": CANARY_KEY, "funded": CANARY_KEY, "error": CANARY_KEY, "key": CANARY_KEY});
        r.openrouter.leaky.store(true, Ordering::SeqCst);
        let mac_token = r.login().await;
        let mac = format!("eve_mac={mac_token}");
        let failed = r.mac("GET", &format!("{OPENROUTER_CALLBACK_PATH}?code=c&state=s"), (), Req::new()).await;
        assert!(failed.status == 502 && !contains(&failed.body, CANARY_KEY.as_bytes()));
        let mut answers = Vec::new();
        for path in [ONBOARD_STATE_PATH, STATUS_PATH, HEALTH_PATH, DEVICES_PATH, APPS_PATH] {
            answers.push((path, r.mac("GET", path, (), Req::new().cookie(&mac)).await));
        }
        for (path, reply) in &answers {
            assert_eq!(reply.status, 200, "{path}");
            let text = reply.text();
            assert!(key_shaped(&text).is_empty(), "{path} {:?}", key_shaped(&text));
            assert!(!text.contains(&mac_token) && !text.contains(&r.ctx.shared.nonce), "{path}");
        }
        assert_eq!(answers[0].1.json()["error"], SIGNIN_BROKE);
        let status = answers[1].1.json();
        assert!(status["state"] == "READY" && status["credit"] == json!({"ok": true, "usd": null, "funded": null}));
        let keys: std::collections::BTreeSet<String> = status.as_object().unwrap().keys().cloned().collect();
        let want: std::collections::BTreeSet<String> =
            ["state", "openrouter", "openrouter_rejected", "composio", "voice", "lan", "local_network", "restarting", "credit"]
                .iter()
                .map(|s| s.to_string())
                .collect();
        assert_eq!(keys, want);
        // positive control: with the credit's guard taken away, the same request leaks, and the scan sees it
        r.ctx.update(|p| p.credit_view = leaky_view);
        r.ctx.shared.forget_credit();
        let leaked = r.mac("GET", STATUS_PATH, (), Req::new().cookie(&mac)).await.text();
        assert!(leaked.contains(CANARY_KEY) && !key_shaped(&leaked).is_empty());
        r.stop().await;
    });
}

// ---- headers -------------------------------------------------------------------------------------------------------

fn assert_secure(reply: &Reply) {
    assert_eq!(reply.header("content-security-policy"), CSP);
    assert!(reply.header("content-security-policy").contains("frame-ancestors 'none'"));
    assert!(reply.header("content-security-policy").contains("script-src 'self';"));
    assert_eq!(reply.header("x-content-type-options"), "nosniff");
    assert_eq!(reply.header("referrer-policy"), "no-referrer");
    assert_eq!(reply.header("permissions-policy"), "microphone=(self), camera=(), geolocation=()");
    assert_eq!(reply.header("cross-origin-resource-policy"), "same-origin");
    assert_eq!(reply.header("connection"), "close");
}

#[test]
fn test_security_headers_are_on_every_answer() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let inlet = FakeInlet::new(Some(store_at(tmp.path())), Some(two_lines()));
        let r = Rig::with(&tmp.path().join("rig"), RigOpts { link: Some(link_door(tmp.path(), inlet)), ..RigOpts::default() }).await;
        let token = r.login().await;
        let mac = format!("eve_mac={token}");
        let page = r.mac("GET", "/", (), Req::new()).await;
        assert_secure(&page);
        assert_eq!(page.header("content-type"), "text/html; charset=utf-8");
        let api = r.mac("GET", STATUS_PATH, (), Req::new().cookie(&mac)).await;
        assert_secure(&api);
        assert!(api.header("cache-control") == "no-store" && api.header("content-type") == "application/json");
        let replies = [
            r.mac("GET", "/nothing", (), Req::new()).await,
            r.mac("GET", STATUS_PATH, (), Req::new()).await,
            r.mac("GET", "/", (), Req::new().host("evil.example")).await,
            r.mac("GET", OPENROUTER_START_PATH, (), Req::new().cookie(&mac)).await,
            r.mac("GET", &format!("{OPENROUTER_CALLBACK_PATH}?code=x&state=y"), (), Req::new()).await,
            r.mac("GET", "/assets/eve-abc123.js", (), Req::new()).await,
        ];
        for reply in &replies {
            assert_secure(reply);
            if reply.status != 200 {
                assert_eq!(reply.header("cache-control"), "no-store");
            }
        }
        r.open_pairing(&token).await;
        for path in [CA_PATH, MOBILECONFIG_PATH, "/"] {
            assert_secure(&r.lan("GET", path, (), Req::new()).await);
        }
        // LinkChat's own answers get the two that cannot break a file it serves inline
        let turn = r.mac("POST", "/api/web/link/turn", json!({"text": "hi", "client_id": "h-1"}), Req::new().cookie(&mac)).await;
        assert!(turn.header("referrer-policy") == "no-referrer" && turn.header("x-content-type-options") == "nosniff");
        assert_eq!(turn.header("cache-control"), "no-store");
        r.stop().await;
    });
}

// ---- what reaches LinkChat ------------------------------------------------------------------------------------------

#[test]
fn test_a_turn_reaches_linkchat_with_origin_removed_and_the_nonce_set() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let inlet = FakeInlet::new(Some(store_at(tmp.path())), Some(two_lines()));
        let spy = SpyLink::new(link_door(tmp.path(), inlet));
        let r = Rig::with(&tmp.path().join("rig"), RigOpts { link: Some(spy.clone()), ..RigOpts::default() }).await;
        let token = r.login().await;
        let nonce = r.ctx.shared.nonce.clone();
        *spy.probes.lock() = vec![nonce.clone(), token.clone(), String::new()];
        let turn = r
            .mac("POST", "/api/web/link/turn", json!({"text": "hello", "client_id": "nonce-1"}),
                 Req::new().cookie(format!("eve_mac={token}")).header("Accept", "text/event-stream"))
            .await;
        assert_eq!(turn.status, 200);
        let (contents, done, _) = events(&turn.body);
        assert!(contents == ["First line.", "\n\nSecond line."] && done); // LinkChat took the nonce
        let seen = spy.seen.lock()[0].clone();
        assert!(seen.method == "POST" && seen.path == "/api/link/turn");
        assert!(!seen.headers.contains_key("origin") && !seen.headers.contains_key("cookie"));
        assert_eq!(seen.headers["authorization"], format!("Bearer {nonce}"));
        assert_eq!(seen.headers["accept"], "text/event-stream"); // the rest as the page sent it
        assert_eq!(seen.checks, [(nonce.clone(), true), (token.clone(), false), (String::new(), false)]);
        assert!(!contains(&turn.body, nonce.as_bytes()));
        // the same from a phone, over the LAN door
        let phone = r.pair(Some(&token)).await;
        assert_eq!(r.lan("GET", "/api/web/link/messages?wait=0", (), Req::new().cookie(format!("eve_dev={phone}"))).await.status, 200);
        let seen = spy.seen.lock().last().unwrap().clone();
        assert!(seen.path == "/api/link/messages" && seen.query == "wait=0");
        assert!(!seen.headers.contains_key("origin") && !seen.headers.contains_key("cookie"));
        assert_eq!(seen.headers["authorization"], format!("Bearer {nonce}"));
        r.stop().await;
    });
}

#[test]
fn test_linkchat_paths_outside_the_allowlist_are_404_at_the_door() {
    let cases = [
        ("GET", "/api/web/link/jobs"), ("POST", "/api/web/link/jobs"), ("POST", "/api/web/link/jobs/c1/go"),
        ("GET", "/api/web/link/apps"), ("GET", "/api/web/link/controls"), ("POST", "/api/web/link/controls/lights"),
        ("GET", "/api/web/link/controls/pika/stream"), ("GET", "/api/web/link/today"), ("GET", "/api/web/link/updates"),
        ("POST", "/api/web/link/updates/refresh"), ("POST", "/api/web/link/location"), ("GET", "/api/web/link/sessions"),
        ("POST", "/api/web/link/sessions/start"), ("GET", "/api/web/link/file/../../etc"),
        ("GET", "/api/web/link/file/ZZ"), ("GET", "/api/web/link/"), ("GET", "/api/web/link/messages/x"),
        ("GET", "/api/link/messages"), ("POST", "/api/link/turn"),
    ];
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let spy = SpyLink::new(link_door(tmp.path(), FakeInlet::new(Some(store_at(tmp.path())), None)));
        let r = Rig::with(&tmp.path().join("rig"), RigOpts { link: Some(spy.clone()), ..RigOpts::default() }).await;
        let token = r.login().await;
        let phone = r.pair(Some(&token)).await;
        for (method, path) in cases {
            assert_eq!(r.mac(method, path, json!({}), Req::new().cookie(format!("eve_mac={token}"))).await.status, 404, "{method} {path}");
            assert_eq!(r.lan(method, path, json!({}), Req::new().cookie(format!("eve_dev={phone}"))).await.status, 404, "{method} {path}");
            assert!(spy.seen.lock().is_empty()); // LinkChat never saw it
        }
        assert_eq!(r.mac("GET", "/api/web/link/messages", (), Req::new().cookie(format!("eve_mac={token}"))).await.status, 200);
        assert_eq!(spy.seen.lock().len(), 1); // positive control
        r.stop().await;
    });
}

#[test]
fn test_wrong_methods_on_the_link_door_never_reach_linkchat() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let spy = SpyLink::new(link_door(tmp.path(), FakeInlet::new(Some(store_at(tmp.path())), None)));
        let r = Rig::with(&tmp.path().join("rig"), RigOpts { link: Some(spy.clone()), ..RigOpts::default() }).await;
        let mac = format!("eve_mac={}", r.login().await);
        for (method, path) in [
            ("GET", "/api/web/link/turn"), ("POST", "/api/web/link/messages"), ("GET", "/api/web/link/upload"),
            ("PUT", "/api/web/link/turn"), ("DELETE", STATUS_PATH), ("POST", STATUS_PATH),
        ] {
            let reply = r.mac(method, path, json!({}), Req::new().cookie(&mac)).await;
            assert_eq!(reply.status, 405, "{method} {path}");
            assert!(!reply.header("allow").is_empty());
        }
        assert!(spy.seen.lock().is_empty());
        let allow = r.mac("PUT", STATUS_PATH, json!({}), Req::new().cookie(&mac)).await;
        assert_eq!(allow.header("allow"), "GET");
        let both = r.mac("POST", "/", json!({}), Req::new()).await;
        assert_eq!(both.header("allow"), "GET, HEAD"); // sorted, as Python's sorted(route.methods)
        r.stop().await;
    });
}

// ---- requests that are not plain HTTP/1.1 ------------------------------------------------------------------------

#[test]
fn test_requests_that_are_not_plain_http_are_refused_before_routing() {
    let many = "X-Many: 1\r\n".repeat(70);
    let big = "a".repeat(70000);
    let long = "a".repeat(9000);
    let cases: Vec<(Vec<u8>, u16)> = vec![
        (b"GET /healthz HTTP/1.1\r\nHost: {host}\r\nHost: {host}\r\n\r\n".to_vec(), 400), // two Hosts
        (b"POST /onboard/composio/skip HTTP/1.1\r\nHost: {host}\r\nContent-Length: 2\r\nContent-Length: 3\r\n\r\n{}".to_vec(), 400),
        (b"GET /healthz HTTP/1.1\r\nHost: {host}\r\nX-A: a\r\n b\r\n\r\n".to_vec(), 400), // an obs-fold
        (b"GET /healthz HTTP/1.1\r\nHost : {host}\r\n\r\n".to_vec(), 400), // a space before the colon
        (b"GET /healthz HTTP/1.1\r\nHost: {host}\r\nX-A: a\nb\r\n\r\n".to_vec(), 400), // a bare LF
        (b"GET /healthz HTTP/1.1\r\nHost: {host}\r\nX-A: a\x00b\r\n\r\n".to_vec(), 400), // a NUL
        (b"GET http://evil.example/healthz HTTP/1.1\r\nHost: {host}\r\n\r\n".to_vec(), 400), // absolute form
        (b"GET /h\xc3\xa9 HTTP/1.1\r\nHost: {host}\r\n\r\n".to_vec(), 400), // not ASCII
        (b"GET /healthz HTTP/2.0\r\nHost: {host}\r\n\r\n".to_vec(), 400),
        (b"get /healthz HTTP/1.1\r\nHost: {host}\r\n\r\n".to_vec(), 400),
        (b"GET /healthz\r\nHost: {host}\r\n\r\n".to_vec(), 400),
        (format!("GET /{long} HTTP/1.1\r\nHost: {{host}}\r\n\r\n").into_bytes(), 414),
        (format!("GET /healthz HTTP/1.1\r\nHost: {{host}}\r\n{many}\r\n").into_bytes(), 431),
        (format!("GET /healthz HTTP/1.1\r\nHost: {{host}}\r\nX-Big: {big}\r\n\r\n").into_bytes(), 431),
    ];
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let r = Rig::start(tmp.path()).await;
        let host = format!("localhost:{}", r.port);
        for (head, status) in cases {
            let raw = replace(&head, b"{host}", host.as_bytes());
            let reply = send_raw(r.port, &raw, None, 20.0).await.unwrap();
            assert!(reply.starts_with(format!("HTTP/1.1 {status} ").as_bytes()), "{:?}", String::from_utf8_lossy(&reply[..80.min(reply.len())]));
            assert!(!r.ctx.shared.saved.composio_skipped());
            let good = send_raw(r.port, format!("GET /healthz HTTP/1.1\r\nHost: {host}\r\n\r\n").as_bytes(), None, 20.0).await.unwrap();
            assert!(good.starts_with(b"HTTP/1.1 200 ")); // positive control
        }
        r.stop().await;
    });
}

fn replace(hay: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < hay.len() {
        if hay[i..].starts_with(from) {
            out.extend_from_slice(to);
            i += from.len();
        } else {
            out.push(hay[i]);
            i += 1;
        }
    }
    out
}

#[test]
fn test_a_client_that_never_finishes_its_head_is_let_go() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let r = Rig::start(tmp.path()).await;
        r.ctx.update(|p| p.limits.head_secs = 0.2);
        let got = send_raw(r.port, b"GET /healthz HTTP/1.1\r\nHost: loc", None, 5.0).await.unwrap();
        assert!(got.is_empty()); // closed, nothing said
        r.stop().await;
    });
}

#[test]
fn test_too_many_connections_at_once_are_closed_at_once() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let r = Rig::start(tmp.path()).await;
        r.ctx.update(|p| p.limits.max_connections = 3);
        let mut idle = Vec::new();
        for _ in 0..3 {
            idle.push(tokio::net::TcpStream::connect(("127.0.0.1", r.port)).await.unwrap());
        }
        until(|| r.local.open_connections() == 3).await; // all three taken by the door
        let closed = answer_to(r.port, format!("GET /healthz HTTP/1.1\r\nHost: localhost:{}\r\n\r\n", r.port).as_bytes()).await;
        assert!(closed.is_empty() && r.local.refused() == 1);
        drop(idle);
        until(|| r.local.open_connections() == 0).await;
        assert_eq!(r.mac("GET", HEALTH_PATH, (), Req::new()).await.status, 200); // room again
        r.stop().await;
    });
}

#[test]
fn test_one_address_cannot_take_every_connection() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let r = Rig::start(tmp.path()).await;
        r.ctx.update(|p| p.limits.max_per_peer = 2);
        let mut idle = Vec::new();
        for _ in 0..2 {
            idle.push(tokio::net::TcpStream::connect(("127.0.0.1", r.port)).await.unwrap());
        }
        until(|| r.local.connections_from("127.0.0.1") == 2).await;
        let closed = answer_to(r.port, format!("GET /healthz HTTP/1.1\r\nHost: localhost:{}\r\n\r\n", r.port).as_bytes()).await;
        assert!(closed.is_empty() && r.local.refused() == 1);
        drop(idle);
        until(|| !r.local.any_peer()).await;
        assert_eq!(r.mac("GET", HEALTH_PATH, (), Req::new()).await.status, 200); // its slots are free again
        assert!(!r.local.any_peer());
        r.stop().await;
    });
}

#[test]
fn test_a_context_has_one_lan_door() {
    let tmp = tempfile::tempdir().unwrap();
    run(async {
        let r = Rig::start(tmp.path()).await;
        r.open_pairing(&r.login().await).await;
        let err = LanServer::new(Arc::clone(&r.ctx)).start().await.unwrap_err();
        assert!(matches!(&err, StartError::Runtime(line) if line.contains("open already")), "{err:?}");
        assert!(r.local.lan_on());
        r.stop().await;
    });
}
