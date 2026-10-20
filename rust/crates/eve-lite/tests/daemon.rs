//! test_lite_daemon.py: eve lite's daemon, without its firewall (the firewall's own boot is boot_firewall.rs).
//!
//! A real LiteDaemon runs on a temporary home under eve lite's environment (the profile applied), with its Mac page on
//! an ephemeral loopback port, the LAN door on loopback when a test opens it, and its IPC socket in a short temporary
//! folder. The parts are the real ones; only what would leave the Mac is replaced: the spend figures' loop, the
//! Composio bridge, the OpenRouter credit read and sign-in exchange (a mock transport), the Local Network alert and
//! probe, and the Mac's name and address. Each test runs in a child process whose every HTTP client goes through a
//! refusing proxy; every daemon test asserts no host was asked for (common/mod.rs).

mod common;

use std::net::IpAddr;
use std::os::unix::fs::FileTypeExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use common::*;
use eve_lite::{LiteDaemon, LiteOptions, Seams};
use parking_lot::Mutex;
use serde_json::{json, Value};

const RESULT: &str = "eve-lite result:";

fn same<A: ?Sized, B: ?Sized>(a: &Arc<A>, b: &Arc<B>) -> bool {
    Arc::as_ptr(a) as *const u8 == Arc::as_ptr(b) as *const u8
}

/// What the brain was lent, as a test reads it back (Python's `lent` dict).
#[derive(Default)]
struct Lent {
    config: Option<eve_brain::telegram::TelegramConfig>,
    memory: Option<Arc<dyn eve_brain::seams::BrainMemory>>,
    chat_log: Option<Arc<eve_chat_log::ChatLog>>,
    watcher: Option<Arc<eve_web::watch::Watcher>>,
    apps: bool,
    thinker: bool,
    brief: Option<String>,
    agent_factory: bool,
    agent_enabled: bool,
    screen: Option<Option<PathBuf>>,
    launch_recent: usize,
    claude_sessions: usize,
    codex_folders: usize,
    launcher: Option<eve_brain::seams::Launcher>,
}

/// make_inlet that keeps what it was lent, then builds the real brain.
fn keeping(lent: Arc<Mutex<Lent>>) -> eve_lite::MakeInlet {
    Arc::new(move |config, options: eve_brain::telegram::inlet::InletOptions| {
        {
            let mut l = lent.lock();
            l.config = Some(config.clone());
            l.memory = options.memory.clone();
            l.chat_log = options.chat_log.clone();
            l.watcher = options.watcher.clone();
            l.apps = options.apps.is_some();
            l.thinker = options.thinker.is_some();
            l.brief = options.brief.as_ref().map(|f| f());
            l.agent_factory = options.agent_factory.is_some();
            l.agent_enabled = options.agent_enabled;
            l.screen = options.screen.as_ref().map(|f| f());
            l.launch_recent = options.launch_recent.as_ref().map_or(99, |f| f().len());
            l.claude_sessions = options.claude_sessions.as_ref().map_or(99, |f| match f() {
                Ok(eve_brain::seams::Sessions::Now(v)) => v.len(),
                _ => 98,
            });
            l.codex_folders = options.codex_folders.as_ref().map_or(99, |f| f().len());
            l.launcher = options.launcher.clone();
        }
        Ok(eve_brain::telegram::make_inlet(config, options))
    })
}

#[test]
fn the_network_guard_catches_a_lookup() {
    // Positive control for the guard every test here ends on: a request from the child's runtime, and one from a
    // blocking worker of it, are refused and seen by the parent.
    const NAME: &str = "the_network_guard_catches_a_lookup";
    if is_child(NAME) {
        child_start(NAME);
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let client = reqwest::Client::new();
            let first = client.get("https://openrouter.ai/").send().await;
            assert!(first.is_err() || first.unwrap().status() == 403);
            let second = tokio::task::spawn_blocking(|| {
                tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
                    reqwest::Client::new().get("http://connect.composio.dev/").send().await.map(|r| r.status().as_u16())
                })
            })
            .await
            .unwrap();
            assert!(second.is_err() || second.unwrap() == 403);
            // this Mac still resolves
            assert!(std::net::ToSocketAddrs::to_socket_addrs("127.0.0.1:80").unwrap().next().is_some());
        });
        return;
    }
    let b = make_box();
    let (asked, _) = run_child(NAME, &b, Child { profile: Some("lite".into()), apply_profile: true, ..Child::default() });
    assert_eq!(asked.hosts(), vec!["openrouter.ai".to_string(), "connect.composio.dev".to_string()]);
}

// ---- READY: only the lite parts --------------------------------------------------------------------------------

#[test]
fn with_a_key_it_builds_only_the_lite_parts_each_once() {
    child_async("with_a_key_it_builds_only_the_lite_parts_each_once", true, || async {
        let stubs = Stubs::default();
        let lent = Arc::new(Mutex::new(Lent::default()));
        let mut options = stubs.options();
        options.seams.make_inlet = keeping(lent.clone());
        let (d, task) = start(options).await;
        assert_eq!(d.state(), eve_lite::READY);
        let inlet = d.inlet().expect("the brain");
        let memory = d.memory().expect("memory");
        let chat_log = d.chat_log().expect("the chat log");
        let watcher = d.watcher().expect("the watcher");
        let launcher = {
            let l = lent.lock();
            // the brain: app-only, eve lite's one chat, lent memory, the chat log, the watcher and think_hard
            let config = l.config.clone().unwrap();
            assert!(config.enabled && config.app_only && config.token.is_empty());
            assert_eq!(config.owner_ids, vec![eve_profile::LITE_CHAT_ID]);
            assert!(same(l.memory.as_ref().unwrap(), &memory));
            assert!(same(l.chat_log.as_ref().unwrap(), &chat_log));
            assert!(memory.chat_log().is_some_and(|c| same(&c, &chat_log)));
            assert!(same(l.watcher.as_ref().unwrap(), &watcher));
            assert!(!l.apps && d.apps().is_none() && stubs.made.all().is_empty());
            assert!(l.thinker && l.brief.is_some());
            // and none of the Mac
            assert!(!l.agent_factory && !l.agent_enabled);
            assert_eq!(l.screen, Some(None));
            assert_eq!((l.launch_recent, l.claude_sessions, l.codex_folders), (0, 0, 0));
            l.launcher.clone().unwrap()
        };
        let refused = launcher(PathBuf::from("/tmp/project"), "claude".into()).await.unwrap();
        assert_eq!(refused, eve_brain::telegram::LITE_NOT_HERE);
        // one chat door, its uploads the brain's; calls on the same brain
        let link = d.link().unwrap();
        assert!(inlet.link_uploads.lock().as_ref().is_some_and(|u| Arc::ptr_eq(u, link.uploads())));
        let calls = d.calls().unwrap();
        assert!(calls.brain().is_some_and(|b| same(&b, &inlet)) && calls.voice().is_some());
        assert_eq!(eve_lite::no_owner("user=%7B%22id%22%3A1%7D&hash=0"), None);
        let rtc = d.rtc().unwrap();
        assert!(rtc.voice_ready() && rtc.stats()["peers"] == json!(0));
        // the page, on loopback only; no LAN door
        let local = d.local().unwrap();
        let web = d.web().unwrap();
        assert!(local.running() && web.shared.lan().is_none());
        assert!(!local.bound().is_empty());
        assert!(local.bound().iter().all(|(addr, _)| addr.parse::<IpAddr>().unwrap().is_loopback()));
        let parts = web.parts();
        assert!(parts.link.is_some_and(|l| same(&l, &link)) && parts.rtc.is_some_and(|r| same(&r, &rtc)));
        assert!(parts.apps.is_none());
        let port = local.port();
        let health = http(port, "GET", eve_lite_web::HEALTH_PATH, None, "").await;
        assert_eq!(health.json(), json!({"ok": true, "state": "READY"}));
        let index = http(port, "GET", "/", None, "").await;
        assert_eq!(index.status, 200);
        assert_eq!(index.body, INDEX);
        // beside them: the brain, the watcher, the spend figures and the dream
        assert_eq!(d.running_tasks(), vec!["lite-brain", "lite-spend-sync", "lite-watch"]);
        assert_eq!(stubs.spend.started(), 1);
        assert_eq!(d.background().names(), vec!["memory-dream"]);
        // the IPC socket, 0600
        let info = std::fs::metadata(sock()).unwrap();
        assert!(info.file_type().is_socket() && mode(&sock()) == 0o600);
        assert_eq!((*stubs.alerts.lock(), stubs.probes.lock().len()), (0, 0)); // nothing asked macOS about the network
        let code = finish(&d, task).await;
        // stopped: the page, the peers, the tasks, the socket
        assert!(!local.running() && rtc.closed());
        assert!(d.tasks_done() && !sock().exists());
        assert_eq!(code, eve_lite::EXIT_OK);
    });
}

#[test]
fn without_a_key_it_is_the_setup_page_and_the_ipc_only() {
    child_async("without_a_key_it_is_the_setup_page_and_the_ipc_only", false, || async {
        let stubs = Stubs::default();
        let built = Arc::new(Mutex::new(Vec::<&str>::new()));
        let mut options = stubs.options();
        let b = built.clone();
        options.seams.make_inlet = Arc::new(move |_, _| {
            b.lock().push("brain");
            Ok(None)
        });
        let b = built.clone();
        options.seams.build_memory = Arc::new(move |_| {
            b.lock().push("memory");
            Ok(None)
        });
        let (d, task) = start(options).await;
        assert_eq!(d.state(), eve_lite::NEEDS_OPENROUTER);
        assert!(d.memory().is_none() && d.chat_log().is_none() && d.watcher().is_none() && d.apps().is_none());
        assert!(d.inlet().is_none() && d.link().is_none() && d.calls().is_none() && d.rtc().is_none());
        let parts = d.web().unwrap().parts();
        assert!(parts.link.is_none() && parts.rtc.is_none() && parts.apps.is_none());
        assert!(d.running_tasks().is_empty() && d.background().names().is_empty());
        let port = d.local().unwrap().port();
        let health = http(port, "GET", eve_lite_web::HEALTH_PATH, None, "").await;
        assert_eq!(health.json(), json!({"ok": true, "state": "NEEDS_OPENROUTER"}));
        let status = ask_json(&sock(), json!({"evt": "status"})).await;
        assert_eq!(status["state"], "NEEDS_OPENROUTER");
        assert_eq!((status["openrouter"].clone(), status["page"].clone()), (json!(false), json!(true)));
        assert_eq!([&status["voice"], &status["apps"], &status["chat"]], [&json!(false), &json!(false), &json!(false)]);
        assert_eq!(status["port"], json!(port));
        let cookie = mac_sign_in(&sock(), port).await;
        let onboard = http(port, "GET", eve_lite_web::ONBOARD_STATE_PATH, None, &cookie).await;
        assert_eq!(onboard.status, 200);
        assert_eq!(onboard.json()["openrouter"], json!(false));
        // the chat door answers that eve is not set up, rather than a chat with no brain
        let turn = http(port, "POST", "/api/web/link/turn", Some(json!({"text": "hi"})), &cookie).await;
        assert_eq!(turn.status, 503);
        assert_eq!(turn.json()["error"], json!(eve_lite_web::NOT_READY));
        assert_eq!(finish(&d, task).await, eve_lite::EXIT_OK);
        assert!(built.lock().is_empty() && stubs.spend.started() == 0);
    });
}

#[test]
fn a_composio_sign_in_builds_the_bridge_and_starts_it() {
    child_async("a_composio_sign_in_builds_the_bridge_and_starts_it", true, || async {
        let token_file = signed_in_to_composio(&home());
        let stubs = Stubs::default();
        let (d, task) = start(stubs.options()).await;
        let made = stubs.made.all();
        assert_eq!(made.len(), 1);
        let bridge = made[0].clone();
        assert!(d.apps().is_some_and(|a| same(&a, &bridge)));
        assert!(d.web().unwrap().parts().apps.is_some_and(|a| same(&a, &bridge)));
        assert_eq!(bridge.token_path, token_file);
        assert_eq!(*bridge.starts.lock(), 1);
        let status = ask_json(&sock(), json!({"evt": "status"})).await;
        assert_eq!((status["composio"].clone(), status["apps"].clone()), (json!(true), json!(true)));
        *bridge.needs_sign_in.lock() = true; // only a new sign-in can help now: Settings says so
        assert_eq!(ask_json(&sock(), json!({"evt": "status"})).await["composio"], json!(false));
        assert_eq!(finish(&d, task).await, eve_lite::EXIT_OK);
        assert!(*bridge.closed.lock());
    });
}

// ---- the IPC ---------------------------------------------------------------------------------------------------

#[test]
fn the_ipc_answers_the_contract_on_a_private_socket() {
    child_async("the_ipc_answers_the_contract_on_a_private_socket", true, || async {
        let stubs = Stubs::default();
        let (d, task) = start(stubs.options()).await;
        let s = sock();
        assert_eq!(mode(&s), 0o600);
        let port = d.local().unwrap().port();
        // web_login: a one-time code and the page's port; web_login_used turns true once a browser used it
        let login = ask_json(&s, json!({"evt": "web_login"})).await;
        let keys: Vec<&String> = login.as_object().unwrap().keys().collect();
        assert_eq!(keys.len(), 3);
        assert_eq!((login["ok"].clone(), login["port"].clone()), (json!(true), json!(port)));
        let code = login["code"].as_str().unwrap().to_string();
        assert!(eve_web_auth::TOKEN.is_match(&code));
        assert_eq!(ask_json(&s, json!({"evt": "web_login_used", "code": code})).await, json!({"ok": true, "used": false}));
        let signed = http(port, "POST", eve_lite_web::MAC_LOGIN_PATH, Some(json!({"code": code})), "").await;
        assert_eq!(signed.status, 200);
        let cookie = signed.cookie(eve_web_auth::COOKIE_MAC);
        assert_eq!(ask_json(&s, json!({"evt": "web_login_used", "code": code})).await, json!({"ok": true, "used": true}));
        let again = http(port, "POST", eve_lite_web::MAC_LOGIN_PATH, Some(json!({"code": code})), "").await;
        assert_eq!(again.status, 403); // single use
        assert_eq!(ask_json(&s, json!({"evt": "web_login_used", "code": "nope"})).await["used"], json!(false));
        // status: words, booleans, ports, the firewall's counts; never the key or a code
        let status = ask_json(&s, json!({"evt": "status"})).await;
        assert_eq!(
            status,
            json!({"ok": true, "state": "READY", "openrouter": true, "openrouter_rejected": false,
                   "chat": true, "composio": false, "apps": false, "voice": true, "page": true,
                   "port": port, "lan": false, "lan_port": 0, "local_network": "unknown",
                   "restarting": false, "firewall": {}})
        );
        // devices: the Mac's browser; revoke by id signs it out
        let devices = ask_json(&s, json!({"evt": "devices"})).await;
        assert_eq!(devices["ok"], json!(true));
        let rows = devices["devices"].as_array().unwrap();
        assert_eq!(rows.iter().map(|r| r["label"].clone()).collect::<Vec<_>>(), vec![json!("This Mac · Safari")]);
        let mut fields: Vec<&String> = rows[0].as_object().unwrap().keys().collect();
        fields.sort();
        assert_eq!(fields, vec!["created", "host", "id", "label", "last_seen"]);
        let mac_id = rows[0]["id"].as_str().unwrap().to_string();
        assert_eq!(http(port, "GET", eve_lite_web::STATUS_PATH, None, &cookie).await.status, 200);
        assert_eq!(
            ask_json(&s, json!({"evt": "revoke", "id": mac_id.to_uppercase()})).await,
            json!({"ok": true, "revoked": [mac_id]})
        );
        assert_eq!(http(port, "GET", eve_lite_web::STATUS_PATH, None, &cookie).await.status, 401);
        assert_eq!(ask_json(&s, json!({"evt": "revoke", "id": "all"})).await, json!({"ok": true, "revoked": []}));
        // refusals: one plain line each, and the daemon goes on
        let device_id = json!({"ok": false, "error": eve_lite_web::DEVICE_ID});
        assert_eq!(ask_json(&s, json!({"evt": "revoke", "id": "../etc"})).await, device_id);
        assert_eq!(ask_json(&s, json!({"evt": "revoke"})).await, device_id);
        assert_eq!(
            ask_json(&s, json!({"evt": "lan", "on": "yes"})).await,
            json!({"ok": false, "error": eve_lite_web::LAN_FLAG})
        );
        let unknown = json!({"ok": false, "error": eve_lite::UNKNOWN_EVENT});
        assert_eq!(ask_json(&s, json!({"evt": "pretooluse", "tool_name": "Bash"})).await, unknown);
        assert_eq!(ask_json(&s, json!(["web_login"])).await, unknown);
        assert_eq!(ask(&s, b"{not json").await["ok"], json!(false));
        assert_eq!(ask_json(&s, json!({"evt": "status"})).await["state"], json!("READY"));
        let text = json!([login, status, devices]).to_string();
        assert!(!text.contains(CANARY_KEY));
        finish(&d, task).await;
        let log = logs();
        assert!(!log.contains(&code), "the code is in a log line");
        assert!(!log.contains(CANARY_KEY));
    });
}

#[test]
fn an_ipc_request_that_fails_is_a_plain_line_and_logs_its_type_only() {
    child_async("an_ipc_request_that_fails_is_a_plain_line_and_logs_its_type_only", false, || async {
        let stubs = Stubs::default();
        let mut options = stubs.options();
        options.seams.list_devices = Arc::new(|_| {
            Err(eve_ipc::NamedError::new("ValueError", "a device list that holds CANARY-NOT-A-KEY-7f3c").into())
        });
        let (d, task) = start(options).await;
        let answer = ask_json(&sock(), json!({"evt": "devices", "code": "CANARY-NOT-A-KEY-7f3c-code"})).await;
        assert_eq!(answer, json!({"ok": false, "error": eve_lite_web::FAILED}));
        assert_eq!(ask_json(&sock(), json!({"evt": "status"})).await["ok"], json!(true)); // and it goes on
        finish(&d, task).await;
        assert!(logs().contains("an IPC request failed (ValueError)"));
        assert!(!logs().contains("CANARY"));
    });
}

#[test]
fn the_lan_door_is_off_until_enabled_and_remembered() {
    child_async("the_lan_door_is_off_until_enabled_and_remembered", true, || async {
        let stubs = Stubs::default();
        let web_json = home().join(".config/eve-bridge/web.json");
        let s = sock();
        {
            let (d, task) = start(stubs.options()).await;
            let web = d.web().unwrap();
            assert!(web.shared.lan().is_none());
            assert_eq!(ask_json(&s, json!({"evt": "status"})).await["lan"], json!(false));
            assert_eq!(ask_json(&s, json!({"evt": "lan", "on": true})).await, json!({"ok": true, "lan": true}));
            let status = ask_json(&s, json!({"evt": "status"})).await;
            assert_eq!(status["lan"], json!(true));
            let lan_port = status["lan_port"].as_u64().unwrap();
            assert!(lan_port > 0 && json!(lan_port) != status["port"]);
            let lan = web.shared.lan().expect("the LAN door");
            assert!(lan.bound().iter().all(|(addr, _)| addr == "127.0.0.1")); // the test's lan_hosts
            let saved: Value = serde_json::from_str(&read(&web_json)).unwrap();
            assert_eq!(saved["lan"], json!(true));
            assert_eq!(mode(&web_json), 0o600);
            finish(&d, task).await;
        }
        {
            let (d, task) = start(stubs.options()).await;
            // remembered: the next start opens it again by itself
            let lan = d.web().unwrap().shared.lan().expect("opened again");
            assert!(lan.running());
            assert_eq!(ask_json(&s, json!({"evt": "status"})).await["lan"], json!(true));
            assert_eq!(ask_json(&s, json!({"evt": "lan", "on": false})).await, json!({"ok": true, "lan": false}));
            assert!(!lan.running());
            let saved: Value = serde_json::from_str(&read(&web_json)).unwrap();
            assert_eq!(saved["lan"], json!(false));
            finish(&d, task).await;
        }
        {
            let (d, task) = start(stubs.options()).await;
            assert!(d.web().unwrap().shared.lan().is_none());
            finish(&d, task).await;
        }
        // turning the door on is not "Add a phone": no Local Network alert
        assert_eq!(*stubs.alerts.lock(), 0);
    });
}

/// A port on loopback held by a listener, and the options that make it the page's only port.
fn held_port(stubs: &Stubs) -> (Arc<Mutex<Option<std::net::TcpListener>>>, u16, LiteOptions) {
    let holder = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let taken = holder.local_addr().unwrap().port();
    let mut options = stubs.options();
    options.web_options = Some(stubs.web_options(Some(Arc::new(move |p: &mut eve_lite_web::Parts| {
        p.local_hosts = vec!["127.0.0.1".into()];
        p.local_ports = vec![taken];
    }))));
    options.seams.page_retry_secs = vec![0.05];
    (Arc::new(Mutex::new(Some(holder))), taken, options)
}

#[test]
fn the_page_comes_up_once_its_port_is_free() {
    child_async("the_page_comes_up_once_its_port_is_free", false, || async {
        let stubs = Stubs::default();
        let (holder, taken, options) = held_port(&stubs);
        let (d, task) = start(options).await;
        let local = d.local().unwrap();
        assert!(!local.running());
        assert_eq!(ask_json(&sock(), json!({"evt": "web_login"})).await, json!({"ok": false, "error": eve_lite::PAGE_DOWN}));
        assert_eq!(ask_json(&sock(), json!({"evt": "status"})).await["page"], json!(false));
        holder.lock().take();
        let watched = local.clone();
        until(move || watched.running(), 10.0).await;
        let login = ask_json(&sock(), json!({"evt": "web_login"})).await;
        assert_eq!((login["ok"].clone(), login["port"].clone()), (json!(true), json!(taken)));
        finish(&d, task).await;
    });
}

#[test]
fn a_stop_during_a_late_page_start_leaves_no_page_open() {
    // The retry's start may be under way when eve stops: it is let finish, then the page is stopped with the rest, so
    // no socket opens after the stop. (Python makes the bind late and the next step slow by patching both; here the
    // stop lands right as the port frees, in the retry's window, and the page must be shut whichever side won.)
    child_async("a_stop_during_a_late_page_start_leaves_no_page_open", true, || async {
        let stubs = Stubs::default();
        let (holder, _, options) = held_port(&stubs);
        let (d, task) = start(options).await;
        holder.lock().take();
        tokio::time::sleep(Duration::from_millis(40)).await;
        d.shutdown();
        assert_eq!(exit_of(task).await, eve_lite::EXIT_OK);
        let local = d.local().unwrap();
        assert!(!local.running() && local.bound().is_empty());
    });
}

// ---- restarts and stopping ------------------------------------------------------------------------------------

/// httpx.MockTransport for the sign-in's key exchange.
struct Provider(Arc<Mutex<Vec<String>>>);

#[async_trait::async_trait]
impl eve_brain::seams::HttpTransport for Provider {
    async fn send(
        &self,
        request: eve_brain::seams::HttpRequest,
    ) -> Result<eve_brain::seams::HttpResponse, eve_brain::seams::HttpError> {
        self.0.lock().push(request.url.clone());
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

#[test]
fn an_openrouter_sign_in_restarts_eve_into_its_key() {
    // The setup page's sign-in, end to end on the real routes: the key is exchanged (a mock transport), saved, and eve
    // stops gracefully and exits 75 for launchd to start it again into READY.
    child_async("an_openrouter_sign_in_restarts_eve_into_its_key", false, || async {
        let stubs = Stubs::default();
        let exchanged = Arc::new(Mutex::new(Vec::new()));
        let mut options = stubs.options();
        options.openrouter_login = Some(Arc::new(eve_brain::onboarding::OpenRouterLogin::new(
            Some(Arc::new(Provider(exchanged.clone()))),
            None,
        )));
        let (d, task) = start(options).await;
        assert_eq!(d.state(), eve_lite::NEEDS_OPENROUTER);
        let port = d.local().unwrap().port();
        let cookie = mac_sign_in(&sock(), port).await;
        let started = http(port, "GET", eve_lite_web::OPENROUTER_START_PATH, None, &cookie).await;
        assert_eq!(started.status, 302);
        let location = started.headers["location"].clone();
        assert!(location.starts_with(eve_brain::onboarding::OPENROUTER_AUTH_URL), "{location}");
        let query = location.split_once('?').unwrap().1;
        let state = query.split('&').find_map(|kv| kv.strip_prefix("state=")).unwrap().to_string();
        let back = http(
            port,
            "GET",
            &format!("{}?code=code-1234&state={state}", eve_lite_web::OPENROUTER_CALLBACK_PATH),
            None,
            "",
        )
        .await;
        assert_eq!(back.status, 303);
        assert_eq!(back.headers["location"], "/");
        let code = exit_of(task).await; // it stops by itself
        assert!(!d.local().unwrap().running() && !sock().exists());
        assert_eq!(code, eve_lite::EXIT_RESTART);
        assert_eq!(*exchanged.lock(), vec![eve_brain::onboarding::OPENROUTER_KEYS_URL.to_string()]);
        let env_file = home().join(".config/eve-bridge/env");
        assert!(read(&env_file).lines().any(|l| l == format!("OPENROUTER_API_KEY={CANARY_KEY}")));
        assert_eq!(mode(&env_file), 0o600);
        assert!(logs().contains("restarts into it (exit 75)"));
        assert!(!logs().contains(CANARY_KEY));
    });
}

#[test]
fn a_restart_asked_twice_is_one_restart() {
    child_async("a_restart_asked_twice_is_one_restart", false, || async {
        let d = LiteDaemon::new(LiteOptions { socket_path: Some("/nonexistent/ipc.sock".into()), ..LiteOptions::default() });
        d.restart();
        d.restart();
        assert!(d.exit_code() == eve_lite::EXIT_RESTART && d.stopping());
        assert_eq!(logs().matches("restarts into it").count(), 1);
        d.shutdown(); // a stop after a restart is still the restart
        assert_eq!(d.exit_code(), eve_lite::EXIT_RESTART);
    });
}

#[test]
fn the_stop_runs_in_order_and_closes_an_open_conversation() {
    child_async("the_stop_runs_in_order_and_closes_an_open_conversation", true, || async {
        signed_in_to_composio(&home());
        let stubs = Stubs::default();
        let (d, task) = start(stubs.options()).await;
        // Composio is closed after the page, the peers and the brain, and before the IPC socket
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let (watched, kept) = (Arc::downgrade(&d), seen.clone());
        let socket = sock();
        *stubs.made.all()[0].on_close.lock() = Some(Box::new(move || {
            let Some(d) = watched.upgrade() else { return };
            let mut kept = kept.lock();
            kept.push(format!("page up: {}", d.local().unwrap().running()));
            kept.push(format!("peers closed: {}", d.rtc().unwrap().closed()));
            kept.push(format!("tasks done: {}", d.tasks_done()));
            kept.push(format!("socket: {}", socket.exists()));
        }));
        let inlet = d.inlet().unwrap();
        inlet.note("user", "a line that was said", "say", None, None, None, false, None);
        let conv = inlet.conversation().expect("a conversation");
        assert_eq!(finish(&d, task).await, eve_lite::EXIT_OK);
        assert_eq!(*seen.lock(), vec!["page up: false", "peers closed: true", "tasks done: true", "socket: true"]);
        let lines = d.memory().unwrap().transcripts.conv_lines(&conv);
        let kinds: Vec<String> =
            lines.iter().map(|l| l.get("kind").and_then(Value::as_str).unwrap_or("").to_string()).collect();
        assert_eq!(kinds.last().map(String::as_str), Some("close"), "{kinds:?}");
        assert_eq!(kinds.iter().filter(|k| *k == "close").count(), 1);
        assert_eq!(inlet.conversation(), None);
    });
}

#[test]
fn the_backstop_closes_a_conversation_left_open() {
    // The daemon's own backstop (Python's test patches the brain's close marker away): a conversation still open
    // after the brain's goodbye is closed once, and one with nothing in it gets nothing.
    child_async("the_backstop_closes_a_conversation_left_open", false, || async {
        let root = home().join("transcripts");
        let store =
            eve_transcripts::Transcripts::new(eve_transcripts::TranscriptConfig::new(true, root), None, None);
        let conv = store.new_conv(eve_brain::telegram::CHANNEL);
        assert!(!eve_lite::close_conversation(&store, &conv)); // nothing in it: nothing written
        assert!(store.append(eve_brain::telegram::CHANNEL, &conv, "owner", "say", "a line that was said"));
        assert!(eve_lite::close_conversation(&store, &conv));
        let kinds: Vec<String> = store
            .conv_lines(&conv)
            .iter()
            .map(|l| l.get("kind").and_then(Value::as_str).unwrap_or("").to_string())
            .collect();
        assert_eq!(kinds.last().map(String::as_str), Some("close"));
        assert_eq!(kinds.iter().filter(|k| *k == "close").count(), 1);
    });
}

#[test]
fn a_stop_from_another_thread_lands_on_the_loop() {
    child_async("a_stop_from_another_thread_lands_on_the_loop", true, || async {
        let stubs = Stubs::default();
        let (d, task) = start(stubs.options()).await;
        let other = d.clone();
        std::thread::spawn(move || other.shutdown()).join().unwrap();
        assert_eq!(exit_of(task).await, eve_lite::EXIT_OK);
    });
}

#[test]
fn a_brain_or_voice_that_cannot_be_built_leaves_the_page_up() {
    // A failure there would crash the daemon, and launchd would start it again every 5 s with no page to reach: it
    // costs the chat and the calls instead, said by type.
    child_async("a_brain_or_voice_that_cannot_be_built_leaves_the_page_up", true, || async {
        let stubs = Stubs::default();
        let mut options = stubs.options();
        options.seams.make_inlet = Arc::new(|_, _| {
            Err(eve_ipc::NamedError::new("RuntimeError", "a part that holds CANARY-NOT-A-KEY-7f3c").into())
        });
        options.seams.make_voice =
            Arc::new(|| Err(eve_ipc::NamedError::new("RuntimeError", "a part that holds CANARY-NOT-A-KEY-7f3c").into()));
        let (d, task) = start(options).await;
        assert!(d.state() == eve_lite::READY && d.inlet().is_none() && d.link().is_some());
        let calls = d.calls().unwrap();
        assert!(calls.brain().is_none() && calls.voice().is_none() && !d.rtc().unwrap().voice_ready());
        let status = ask_json(&sock(), json!({"evt": "status"})).await;
        assert_eq!([&status["chat"], &status["voice"], &status["page"]], [&json!(false), &json!(false), &json!(true)]);
        let port = d.local().unwrap().port();
        let cookie = mac_sign_in(&sock(), port).await;
        let turn = http(port, "POST", "/api/web/link/turn", Some(json!({"text": "hi"})), &cookie).await;
        assert_eq!(turn.status, 503);
        assert_eq!(turn.json()["error"], json!(eve_link::link_chat::NO_CHAT));
        assert_eq!(d.running_tasks(), vec!["lite-spend-sync"]);
        finish(&d, task).await;
        let log = logs();
        assert!(log.contains("the chat brain could not be built (RuntimeError)"));
        assert!(log.contains("the voice could not be built (RuntimeError)"));
        assert!(!log.contains("CANARY"));
    });
}

#[test]
fn memory_that_cannot_open_leaves_eve_ready_without_it() {
    child_async("memory_that_cannot_open_leaves_eve_ready_without_it", true, || async {
        let stubs = Stubs::default();
        let mut options = stubs.options();
        options.seams.build_memory =
            Arc::new(|_| Err(eve_ipc::NamedError::new("OSError", "a disk that holds CANARY-NOT-A-KEY-7f3c").into()));
        let (d, task) = start(options).await;
        assert!(d.state() == eve_lite::READY && d.memory().is_none() && d.inlet().is_some());
        assert!(d.background().names().is_empty()); // no memory: no dream
        finish(&d, task).await;
        assert!(logs().contains("memory: could not open (OSError)") && !logs().contains("CANARY"));
    });
}

// ---- small parts ---------------------------------------------------------------------------------------------

#[test]
fn the_ipc_takes_a_socket_file_only() {
    child_async("the_ipc_takes_a_socket_file_only", false, || async {
        let default = home().join(".eve-lite").join("ipc.sock");
        assert_eq!(PathBuf::from(std::env::var("EVE_BRIDGE_SOCK").unwrap()), default); // apply's default
        assert_eq!(eve_lite::ipc_path(None), default);
        std::env::remove_var("EVE_BRIDGE_SOCK");
        assert_eq!(eve_lite::ipc_path(None), default);
        assert_eq!(eve_lite::ipc_path(Some("/tmp/elsewhere.sock")), PathBuf::from("/tmp/elsewhere.sock"));
        for tcp in ["127.0.0.1:48765", ":48765", "0.0.0.0:48765"] {
            assert_eq!(eve_lite::ipc_path(Some(tcp)), default, "{tcp}");
        }
        std::env::set_var("EVE_BRIDGE_SOCK", "localhost:48765");
        assert_eq!(eve_lite::ipc_path(None), default);
        assert_eq!(logs().matches("not a TCP port").count(), 4);
    });
}

#[test]
fn full_eves_launch_agent_is_found_by_its_label() {
    child_async("full_eves_launch_agent_is_found_by_its_label", false, || async {
        assert_eq!(eve_lite::FULL_LABEL, eve_service::service_launchd::LABEL);
        assert!(!eve_lite::full_eve_installed());
        let plist = home().join("Library/LaunchAgents").join(format!("{}.plist", eve_service::service_launchd::LABEL));
        std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
        std::fs::write(&plist, b"<plist/>").unwrap();
        assert!(eve_lite::full_eve_installed());
    });
}

// ---- the process (each in a fresh child: run_daemon installs the firewall, which cannot be removed) -------------

/// The child's body: run_daemon as `eve-bridge daemon` runs it, then whether a program may still start.
fn process_child(name: &str) -> bool {
    if !is_child(name) {
        return false;
    }
    child_start(name);
    let web: eve_lite::WebOptions = Arc::new(|p: &mut eve_lite_web::Parts| {
        p.local_ports = vec![0];
        p.lan_ports = vec![0];
    });
    let code = eve_lite::run_daemon(None, LiteOptions { web_options: Some(web), seams: Seams::default(), ..LiteOptions::default() });
    let ran = eve_profile::firewall::status(&mut Command::new("/usr/bin/true")).is_ok();
    eprintln!("{RESULT} {}", json!({"exit": code, "program_ran": ran}));
    true
}

fn run_process(b: &Box_, profile: Option<&str>) -> (Value, String) {
    let child = Child {
        profile: profile.map(str::to_string),
        env: vec![("EVE_BRIDGE_SOCK".into(), b.sock.to_string_lossy().into_owned())],
        ..Child::default()
    };
    let name = std::thread::current().name().unwrap().to_string();
    let (_, log) = run_child(&name, b, child);
    let line = log.lines().rev().find_map(|l| l.strip_prefix(RESULT)).expect("a result line");
    (serde_json::from_str(line.trim()).unwrap(), log)
}

#[test]
fn full_eve_installed_keeps_eve_lite_down() {
    if process_child("full_eve_installed_keeps_eve_lite_down") {
        return;
    }
    let b = make_box();
    let plist = b.home.join("Library/LaunchAgents").join(format!("{}.plist", eve_lite::FULL_LABEL));
    std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
    std::fs::write(&plist, b"<plist/>").unwrap();
    let (out, log) = run_process(&b, Some("lite"));
    assert_eq!(out, json!({"exit": eve_lite::EXIT_OK, "program_ran": false})); // exit 0: launchd leaves it down
    assert!(log.contains("full eve's LaunchAgent") && log.contains("eve lite stays down"));
    assert!(!b.sock.exists() && !b.home.join(".config").exists()); // nothing started, nothing written
}

#[test]
fn another_profile_is_exit_78_with_no_firewall() {
    if process_child("another_profile_is_exit_78_with_no_firewall") {
        return;
    }
    let b = make_box();
    let (out, log) = run_process(&b, None);
    assert_eq!(out, json!({"exit": eve_lite::EXIT_CONFIG, "program_ran": true})); // full eve: no firewall installed
    assert!(log.contains("EVE_PROFILE is not lite"));
    let (out, _) = run_process(&b, Some("mini"));
    assert_eq!(out, json!({"exit": eve_lite::EXIT_CONFIG, "program_ran": true}));
    assert!(!b.sock.exists());
}

#[test]
fn a_socket_in_use_is_exit_2() {
    if process_child("a_socket_in_use_is_exit_2") {
        return;
    }
    let b = make_box();
    let other = std::os::unix::net::UnixListener::bind(&b.sock).unwrap();
    let (out, log) = run_process(&b, Some("lite"));
    drop(other);
    assert_eq!(out["exit"], json!(eve_lite::EXIT_IN_USE));
    assert!(log.contains("already listening"));
    assert!(!b.home.join(".config/eve-bridge/web.json").exists()); // no page was started
}
