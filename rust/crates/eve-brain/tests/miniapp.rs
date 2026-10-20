//! Unit `miniapp`: miniapp.py 1-1172; tests: test_miniapp.py, test_miniapp_wheel.py.
//!
//! The shared fakes are in `support` (architect-owned); this unit's own go here.
//!
//! test_miniapp.py's Playwright tests (the home page in Chromium) and all of test_miniapp_wheel.py drive a real
//! browser through Playwright, which the Rust tests have no crate for; the page they drive is the same embedded
//! file (`miniapp::PAGE`), checked here for what can be checked without one (its scripts parse, apps reach it only
//! as text).

mod support;

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::FutureExt;
use hmac::{Hmac, Mac};
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use eve_brain::apps_maker::{AppInfo, Check, Generate, Generated, MakerProgress};
use eve_brain::miniapp::{
    self, app_token, check_init_data, cost_usd, Bot, MiniApp, MiniAppConfig, MiniAppServer, SpendLedger, Tunnel,
    TunnelFactory,
};
use eve_brain::prelude::{wall, Flag, Lent};

type Obj = Map<String, Value>;

const TOKEN: &str = "123456:TEST-token";
const OWNER: i64 = 4242;

// ---- the conftest: every test's spend lines go to a temporary folder --------------------------------------------

static SPEND: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// conftest.py `_spend_ledger_in_tmp`: eve_spend's folder is global, so the tests that touch it take turns.
struct SpendDir {
    _turn: std::sync::MutexGuard<'static, ()>,
    dir: tempfile::TempDir,
}

impl SpendDir {
    fn new() -> Self {
        let turn = SPEND.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().expect("tmp");
        std::fs::create_dir_all(dir.path().join("spend")).expect("spend dir");
        eve_spend::set_dir(Some(dir.path().join("spend")));
        Self { _turn: turn, dir }
    }
    fn folder(&self) -> PathBuf {
        self.dir.path().join("spend")
    }
}

impl Drop for SpendDir {
    fn drop(&mut self) {
        eve_spend::set_dir(None);
    }
}

// ---- signing (tests/initdata.py) --------------------------------------------------------------------------------

/// `urllib.parse.quote_plus`.
fn quote_plus(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn hmac256(key: &[u8], msg: &[u8]) -> Vec<u8> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).unwrap();
    mac.update(msg);
    mac.finalize().into_bytes().to_vec()
}

/// initdata.py `sign_init_data`.
fn sign_init_data(fields: &[(&str, String)], token: &str) -> String {
    let mut sorted: Vec<&(&str, String)> = fields.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(b.0));
    let check = sorted.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("\n");
    let secret = hmac256(b"WebAppData", token.as_bytes());
    let hash = hex::encode(hmac256(&secret, check.as_bytes()));
    let mut parts: Vec<String> = fields.iter().map(|(k, v)| format!("{}={}", quote_plus(k), quote_plus(v))).collect();
    parts.push(format!("hash={hash}"));
    parts.join("&")
}

fn signed_full(user_id: i64, auth_date: Option<i64>, token: &str) -> String {
    let auth = auth_date.unwrap_or(wall() as i64);
    // json.dumps({"id": user_id, "first_name": "O"})
    let user = format!("{{\"id\": {user_id}, \"first_name\": \"O\"}}");
    sign_init_data(&[("auth_date", auth.to_string()), ("query_id", "AAH".into()), ("user", user)], token)
}

fn signed() -> String {
    signed_full(OWNER, None, TOKEN)
}

fn signed_by(user: i64) -> String {
    signed_full(user, None, TOKEN)
}

// ---- who may ask ----------------------------------------------------------------------------------------------

#[test]
fn a_fresh_owner_signature_is_accepted() {
    assert_eq!(check_init_data(&signed(), TOKEN, &[OWNER], None, miniapp::INIT_DATA_MAX_AGE_SECS), Some(OWNER));
}

#[test]
fn a_tampered_field_breaks_the_signature() {
    let good = signed();
    assert_eq!(check_init_data(&good.replace("AAH", "AAX"), TOKEN, &[OWNER], None, miniapp::INIT_DATA_MAX_AGE_SECS), None);
}

#[test]
fn someone_else_signed_by_telegram_is_still_refused() {
    assert_eq!(check_init_data(&signed_by(999), TOKEN, &[OWNER], None, miniapp::INIT_DATA_MAX_AGE_SECS), None);
}

#[test]
fn another_bots_signature_is_refused() {
    let other = signed_full(OWNER, None, "999:OTHER");
    assert_eq!(check_init_data(&other, TOKEN, &[OWNER], None, miniapp::INIT_DATA_MAX_AGE_SECS), None);
}

#[test]
fn a_stale_or_future_signature_is_refused() {
    for age in [miniapp::INIT_DATA_MAX_AGE_SECS as i64 + 60, -3600] {
        let raw = signed_full(OWNER, Some(wall() as i64 - age), TOKEN);
        assert_eq!(check_init_data(&raw, TOKEN, &[OWNER], None, miniapp::INIT_DATA_MAX_AGE_SECS), None, "{age}");
    }
}

#[test]
fn garbage_is_refused() {
    for raw in ["", "user=%7B%7D", "hash=abc", "not a query string at all&&&"] {
        assert_eq!(check_init_data(raw, TOKEN, &[OWNER], None, miniapp::INIT_DATA_MAX_AGE_SECS), None, "{raw}");
    }
}

#[test]
fn an_app_token_opens_its_own_app_until_it_expires() {
    let now = 1_800_000_000.0;
    let tok = app_token("habit-tracker", TOKEN, Some(now));
    assert!(miniapp::check_app_token(&tok, "habit-tracker", TOKEN, Some(now)));
    assert!(!miniapp::check_app_token(&tok, "reading-list", TOKEN, Some(now)));
    assert!(!miniapp::check_app_token(&tok, "habit-tracker", "999:OTHER", Some(now)));
    assert!(!miniapp::check_app_token(&tok, "habit-tracker", TOKEN, Some(now + miniapp::APP_TOKEN_SECS + 1.0)));
    assert!(!miniapp::check_app_token(&format!("9999999999.{}", "0".repeat(40)), "habit-tracker", TOKEN, Some(now)));
    assert!(!miniapp::check_app_token("", "habit-tracker", TOKEN, Some(now)));
    assert_eq!(tok.split_once('.').unwrap().1.len(), 40);
}

// ---- what it costs --------------------------------------------------------------------------------------------

fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-6 * b.abs().max(1.0)
}

#[test]
fn cost_is_the_list_price_per_million_tokens() {
    let usage = json!({"input_tokens": 1_000_000, "output_tokens": 1_000_000, "cache_read_input_tokens": 1_000_000,
                       "cache_creation_input_tokens": 0});
    assert!(approx(cost_usd("claude-opus-5-5", &usage), 4.0 + 20.0 + 0.20));
}

#[test]
fn hour_long_cache_writes_cost_twice_the_input_price() {
    let usage = json!({"input_tokens": 0, "output_tokens": 0, "cache_creation_input_tokens": 3_000_000,
                       "cache_creation": {"ephemeral_5m_input_tokens": 1_000_000, "ephemeral_1h_input_tokens": 2_000_000}});
    assert!(approx(cost_usd("claude-opus-5-5", &usage), 5.0 + 2.0 * 8.0));
    assert!(approx(cost_usd("claude-opus-5-5", &json!({"cache_creation_input_tokens": 1_000_000})), 5.0));
}

#[test]
fn an_unknown_model_costs_at_the_worst_price() {
    let usage = json!({"input_tokens": 1_000_000, "output_tokens": 0});
    assert!(cost_usd("some-future-model", &usage) >= cost_usd("claude-opus-5-5", &usage));
}

fn day_clock(day: &Arc<Mutex<String>>) -> Option<Arc<dyn Fn() -> String + Send + Sync>> {
    let day = day.clone();
    Some(Arc::new(move || day.lock().clone()))
}

#[test]
fn the_ledger_tracks_every_day_and_never_refuses_without_a_cap() {
    let tmp = tempfile::tempdir().unwrap();
    let day = Arc::new(Mutex::new("2026-09-24".to_string()));
    let led = SpendLedger::new(&tmp.path().join("spend.json"), None, day_clock(&day));
    assert!(led.cap.is_none() && led.left().is_none() && !led.over());
    led.add(40.0);
    led.add(0.7);
    // owner: "no claude limit, just track spend"
    assert!(approx(led.spent(), 40.7) && !led.over());
    *day.lock() = "2026-09-25".into();
    led.add(0.3);
    assert!(approx(led.spent(), 0.3));
    let days = SpendLedger::new(&tmp.path().join("spend.json"), None, None).days();
    assert_eq!(days.len(), 2);
    assert!(approx(days["2026-09-24"], 40.7) && approx(days["2026-09-25"], 0.3));
}

#[test]
fn a_cap_of_zero_or_less_is_no_cap() {
    for cap in [Some(0.0), Some(-1.0), None] {
        let tmp = tempfile::tempdir().unwrap();
        let led = SpendLedger::new(&tmp.path().join("spend.json"), cap, None);
        led.add(1000.0);
        assert!(led.cap.is_none() && !led.over(), "{cap:?}");
    }
}

#[test]
fn a_cap_set_by_the_owner_counts_today_and_resets_tomorrow() {
    let tmp = tempfile::tempdir().unwrap();
    let day = Arc::new(Mutex::new("2026-09-24".to_string()));
    let led = SpendLedger::new(&tmp.path().join("spend.json"), Some(1.0), day_clock(&day));
    assert_eq!(led.left(), Some(1.0));
    assert!(!led.over());
    led.add(0.4);
    led.add(0.7);
    assert!(approx(led.spent(), 1.1) && led.left() == Some(0.0) && led.over());
    *day.lock() = "2026-09-25".into();
    assert_eq!(led.left(), Some(1.0));
    assert!(!led.over());
}

#[test]
fn the_ledger_reads_the_capped_versions_file_and_keeps_a_bounded_history() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("spend.json");
    // the format before tracking
    std::fs::write(&path, r#"{"date": "2026-09-24", "usd": 2.5}"#).unwrap();
    let led = SpendLedger::new(&path, None, Some(Arc::new(|| "2026-09-24".to_string())));
    assert!(approx(led.spent(), 2.5));
    led.add(0.5);
    let written: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(written, json!({"days": {"2026-09-24": 3.0}}));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), r#"{"days": {"2026-09-24": 3.0}}"#);
    let mut days = Map::new();
    for d in 1..32 {
        days.insert(format!("2020-01-{d:02}"), json!(1.0));
    }
    days.insert("bad".into(), json!("x"));
    std::fs::write(&path, json!({"days": days}).to_string()).unwrap();
    assert!(!SpendLedger::new(&path, None, None).days().contains_key("bad"));
    for broken in ["not json", "[1, 2]", r#"{"days": 3}"#] {
        std::fs::write(&path, broken).unwrap();
        let led = SpendLedger::new(&path, None, None);
        assert!(led.days().is_empty() && led.spent() == 0.0, "{broken}");
    }
}

#[test]
fn the_ledger_keeps_only_the_newest_days() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("spend.json");
    for d in 1..6 {
        SpendLedger::new(&path, None, Some(Arc::new(move || format!("2026-09-{d:02}")))).keep_days(3).add(1.0);
    }
    let mut days: Vec<String> = SpendLedger::new(&path, None, None).days().into_keys().collect();
    days.sort();
    assert_eq!(days, ["2026-09-03", "2026-09-04", "2026-09-05"]);
}

#[test]
fn each_spend_mark_crossed_is_a_line_and_nothing_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let led = SpendLedger::new(&tmp.path().join("s.json"), None, Some(Arc::new(|| "2026-09-24".to_string())));
    let marks: Arc<Mutex<Vec<(f64, f64)>>> = Arc::default();
    {
        let marks = marks.clone();
        *led.on_cross.lock() = Some(Arc::new(move |mark, total| marks.lock().push((mark, total))));
    }
    led.add(4.0);
    // past 5
    led.add(2.0);
    led.add(1.0);
    // past 20 and 50 at once: one line, the higher mark
    led.add(60.0);
    assert_eq!(*marks.lock(), vec![(5.0, 6.0), (50.0, 67.0)]);
    assert!(!led.over());
}

#[test]
fn a_note_that_panics_never_loses_the_record() {
    let tmp = tempfile::tempdir().unwrap();
    let led = SpendLedger::new(&tmp.path().join("s.json"), None, Some(Arc::new(|| "2026-09-24".to_string())));
    *led.on_cross.lock() = Some(Arc::new(|_, _| panic!("the chat is down")));
    assert!(approx(led.add(6.0), 6.0));
    assert!(approx(led.spent(), 6.0));
}

// ---- the config -------------------------------------------------------------------------------------------------

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

fn base_env() -> Vec<(&'static str, String)> {
    vec![("EVE_TELEGRAM", "1".into()), ("EVE_TELEGRAM_TOKEN", TOKEN.into()), ("EVE_TELEGRAM_OWNER", OWNER.to_string())]
}

fn with(extra: &[(&'static str, &str)]) -> HashMap<String, String> {
    let mut out: HashMap<String, String> = base_env().into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    for (k, v) in extra {
        out.insert((*k).to_string(), (*v).to_string());
    }
    out
}

#[test]
fn off_unless_asked_and_fully_configured() {
    assert!(!miniapp::configured(Some(&with(&[]))).enabled);
    // no API key
    assert!(!miniapp::configured(Some(&with(&[("EVE_MINIAPP", "1")]))).enabled);
    assert!(!miniapp::configured(Some(&with(&[("EVE_MINIAPP", "1"), ("ANTHROPIC_API_KEY", "k")]))).enabled);
    let cfg = miniapp::configured(Some(&with(&[("EVE_MINIAPP", "1"), ("OPENROUTER_API_KEY", "k")])));
    assert!(cfg.enabled && cfg.model == "claude-opus-5-5" && cfg.make_effort == "high");
    assert_eq!(cfg.owner_ids, vec![OWNER]);
    assert!(!format!("{cfg:?}").contains(TOKEN) && !format!("{cfg}").contains(TOKEN));
}

#[test]
fn there_is_no_spend_cap_unless_one_is_set() {
    for (raw, cap) in [(None, 0.0), (Some(""), 0.0), (Some("0"), 0.0), (Some("-3"), 0.0), (Some("lots"), 0.0), (Some("7.5"), 7.5)] {
        let mut e = with(&[("EVE_MINIAPP", "1"), ("OPENROUTER_API_KEY", "k")]);
        if let Some(raw) = raw {
            e.insert("EVE_MINIAPP_DAILY_USD".into(), raw.into());
        }
        assert_eq!(miniapp::configured(Some(&e)).daily_usd, cap, "{raw:?}");
    }
}

#[test]
fn an_off_mini_app_is_the_default_config() {
    let cfg = miniapp::configured(Some(&env(&[("EVE_MINIAPP", "0"), ("OPENROUTER_API_KEY", "k")])));
    assert!(!cfg.enabled && cfg.token.is_empty() && cfg.owner_ids.is_empty());
    assert!(cfg.ledger_path.ends_with(".config/eve-bridge/miniapp-spend.json"));
}

#[test]
fn the_tunnel_url_pattern_matches_cloudflared_output() {
    let line = "2026-09-24T07:30:00Z INF |  https://brave-cat-lamp-owl.trycloudflare.com                    |";
    assert_eq!(miniapp::TUNNEL_URL_RE.find(line).unwrap().as_str(), "https://brave-cat-lamp-owl.trycloudflare.com");
}

#[test]
fn a_phone_report_is_one_short_line() {
    let body: Obj = serde_json::from_value(json!({"platform": "ios", "version": "9.1", "bridge": "proxy",
        "main_button": "shown \"New group\"", "errors": vec!["TypeError: x is null\nsecond line @12"; 7]})).unwrap();
    let line = miniapp::phone_report(&body);
    assert!(line.starts_with("platform=ios, version=9.1, bridge=proxy, main_button=shown \"New group\", errors=7 | "));
    assert!(!line.contains('\n') && line.matches(" | ").count() == 5);
    let none = miniapp::phone_report(&Obj::new());
    assert_eq!(none, "platform=, version=, bridge=, main_button=, errors=0");
}

// ---- the server -------------------------------------------------------------------------------------------------

struct Resp {
    status: u16,
    headers: HashMap<String, String>,
    text: String,
    bytes: Vec<u8>,
}

impl Resp {
    fn json(&self) -> Value {
        serde_json::from_str(&self.text).unwrap_or_else(|e| panic!("not JSON ({e}): {}", self.text))
    }
    fn header(&self, name: &str) -> String {
        self.headers.get(name).cloned().unwrap_or_default()
    }
}

/// httpx's `c.request(method, path, json=body, headers=...)`; redirects are not followed.
async fn raw(port: i64, method: &str, path: &str, body: Option<Value>, headers: &[(&str, &str)]) -> Resp {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap();
    let mut req = client.request(method.parse().unwrap(), format!("http://127.0.0.1:{port}{path}"));
    if let Some(body) = body {
        req = req.json(&body);
    }
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let r = req.send().await.expect("request");
    let status = r.status().as_u16();
    let headers = r.headers().iter().map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string())).collect();
    let bytes = r.bytes().await.map(|b| b.to_vec()).unwrap_or_default();
    Resp { status, headers, text: String::from_utf8_lossy(&bytes).into_owned(), bytes }
}

async fn post(port: i64, path: &str, body: Value) -> Resp {
    raw(port, "POST", path, Some(body), &[]).await
}

async fn get(port: i64, path: &str) -> Resp {
    raw(port, "GET", path, None, &[]).await
}

fn events(body: &str) -> Vec<Value> {
    body.split("\n\n").filter_map(|chunk| chunk.strip_prefix("data: ")).map(|j| serde_json::from_str(j).unwrap()).collect()
}

fn config(dir: &Path, cap: f64) -> MiniAppConfig {
    MiniAppConfig {
        enabled: true,
        token: TOKEN.into(),
        owner_ids: vec![OWNER],
        daily_usd: cap,
        ledger_path: dir.join("spend.json"),
        ..MiniAppConfig::default()
    }
}

fn run_server(dir: &Path, cap: f64) -> Arc<MiniAppServer> {
    let cfg = config(dir, cap);
    let ledger = Arc::new(SpendLedger::new(&cfg.ledger_path, Some(cap), None));
    MiniAppServer::new(&cfg, ledger, Some(b"<html>PAGE</html>"), None, None, None, None)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_page_is_served_and_unknown_paths_are_not() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let srv = run_server(tmp.path(), 0.0);
    let port = srv.start(0).await.unwrap();
    let (page, missing, health) = (get(port, "/").await, get(port, "/etc/passwd").await, get(port, "/healthz").await);
    let eve_js = get(port, "/eve.js").await;
    srv.close().await;
    assert!(page.status == 200 && page.text.contains("PAGE") && missing.status == 404);
    assert_eq!(page.header("content-type"), "text/html; charset=utf-8");
    assert_eq!(page.header("cache-control"), "no-store");
    assert!(health.status == 200 && health.text == "ok");
    // no apps, no eve.js
    assert_eq!(eve_js.status, 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_real_home_page_is_the_embedded_file() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path(), 0.0);
    let srv = MiniAppServer::new(&cfg, Arc::new(SpendLedger::new(&cfg.ledger_path, None, None)), None, None, None, None, None);
    let port = srv.start(0).await.unwrap();
    let page = get(port, "/index.html").await;
    srv.close().await;
    assert_eq!(page.bytes, miniapp::PAGE);
    assert_eq!(std::fs::read(miniapp::PAGE_PATH).unwrap(), miniapp::PAGE);
}

/// iro.js comes from eve itself, byte for byte the npm 5.5.2 release (a changed file fails here).
#[tokio::test(flavor = "multi_thread")]
async fn the_colour_wheel_script_is_served_as_vendored() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let srv = run_server(tmp.path(), 0.0);
    let port = srv.start(0).await.unwrap();
    let r = get(port, "/iro.js").await;
    srv.close().await;
    assert!(r.status == 200 && r.header("content-type").starts_with("application/javascript"));
    assert_eq!(hex::encode(Sha256::digest(&r.bytes)), "5d08eedbac9af7212f5fdf7e336aeb2da87ac47b2364818ad4bbd7fcbdd18d0d");
    assert!(r.bytes.starts_with(b"/*!\n * iro.js v5.5.2"));
}

#[tokio::test(flavor = "multi_thread")]
async fn anyone_else_gets_nothing_and_the_chat_route_is_gone() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let srv = run_server(tmp.path(), 0.0);
    let port = srv.start(0).await.unwrap();
    let mut refused = Vec::new();
    for d in [String::new(), signed_by(999), signed().replace("AAH", "AAB")] {
        refused.push(post(port, "/api/me", json!({"initData": d})).await.status);
    }
    let chat = post(port, "/api/chat", json!({"initData": signed(), "messages": [{"role": "user", "content": "hi"}]})).await;
    let me_get = get(port, "/api/me").await;
    srv.close().await;
    assert_eq!(refused, [403, 403, 403]);
    // the Claude chat was removed (owner, 2026-09-24)
    assert_eq!(chat.status, 404);
    assert_eq!(me_get.status, 404);
}

/// A connection's raw request and its whole answer (the server closes after one).
async fn raw_tcp(port: i64, request: &[u8]) -> Vec<u8> {
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port as u16)).await.unwrap();
    s.write_all(request).await.unwrap();
    let mut out = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(10), s.read_to_end(&mut out)).await;
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn a_body_too_large_or_not_json_is_refused() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let srv = run_server(tmp.path(), 0.0);
    let port = srv.start(0).await.unwrap();
    let huge = raw_tcp(port, b"POST /api/me HTTP/1.1\r\nHost: x\r\nContent-Length: 2000001\r\n\r\n").await;
    let empty = raw_tcp(port, b"POST /api/me HTTP/1.1\r\nHost: x\r\n\r\n").await;
    let list = raw_tcp(port, b"POST /api/me HTTP/1.1\r\nHost: x\r\nContent-Length: 6\r\n\r\n[1, 2]").await;
    let bad_len = raw_tcp(port, b"POST /api/me HTTP/1.1\r\nHost: x\r\nContent-Length: lots\r\n\r\n").await;
    srv.close().await;
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    assert!(text(&huge).starts_with("HTTP/1.1 413 Payload Too Large\r\n"), "{}", text(&huge));
    assert!(text(&empty).starts_with("HTTP/1.1 413 "));
    assert!(text(&list).starts_with("HTTP/1.1 400 Bad Request\r\n") && text(&list).ends_with("bad request"));
    // int() raised: closed without an answer
    assert!(bad_len.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn me_reports_all_of_todays_spend_the_builds_share_and_the_cap_if_any() {
    for (cap, want) in [(0.0, Value::Null), (2.0, json!(2.0))] {
        let _spend = SpendDir::new();
        let tmp = tempfile::tempdir().unwrap();
        let srv = run_server(tmp.path(), cap);
        // the app builder's own ledger
        srv.ledger.add(0.5);
        // the chat, metered by spend.py
        eve_spend::record("openai", "gpt-6-luna", eve_spend::CHAT, Some(0.25), eve_spend::RecordOpts::new());
        eve_spend::record("anthropic", "claude-opus-5-5", eve_spend::APPS, Some(0.5), eve_spend::RecordOpts::new());
        let port = srv.start(0).await.unwrap();
        let r = post(port, "/api/me", json!({"initData": signed()})).await;
        srv.close().await;
        let j = r.json();
        assert!(r.status == 200 && j["cap"] == want, "{}", r.text);
        // ALL of eve today, not only the builds
        assert!(approx(j["spent_today"].as_f64().unwrap(), 0.75));
        assert!(approx(j["apps_today"].as_f64().unwrap(), 0.5));
        assert!(j.get("left").is_none());
        assert_eq!(j["lights"], json!([]));
        assert!(j["apps"] == json!(false) && j["spotify"] == json!(false) && j["firetv"] == json!(false));
        assert_eq!(j["model"], "claude-opus-5-5");
    }
}

#[test]
fn an_app_build_round_is_metered_and_still_costs_the_cap_price() {
    let spend = SpendDir::new();
    let usage = json!({"input_tokens": 1_000_000, "output_tokens": 1_000_000});
    assert!(approx(miniapp::meter_build_for("claude-opus-5-5", &usage), 24.0));
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let rows = eve_spend::day_rows(&today, Some(&spend.folder()));
    assert_eq!((rows[0]["p"].as_str(), rows[0]["f"].as_str()), (Some("openrouter"), Some("app builder")));
    assert!(approx(rows[0]["usd"].as_f64().unwrap(), 24.0));
    // the cap never under-counts
    assert!(miniapp::meter_build_for("claude-future-9", &usage) > 0.0);
    // the meter never guesses
    assert!(eve_spend::day_rows(&today, Some(&spend.folder())).last().unwrap()["usd"].is_null());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_spending_route_is_owner_only_and_carries_aggregates_not_words() {
    let _spend = SpendDir::new();
    let mut tokens = eve_spend::RecordOpts::new();
    tokens.tokens = vec![("in".into(), json!(1000))];
    eve_spend::record("openai", "gpt-6-luna", eve_spend::CHAT, Some(0.25), tokens);
    let mut note = eve_spend::RecordOpts::new();
    note.note = "ChatGPT plan, not per-call (computer task)".into();
    eve_spend::record("chatgpt", "codex", eve_spend::CODEX, None, note);
    let tmp = tempfile::tempdir().unwrap();
    let srv = run_server(tmp.path(), 0.0);
    let port = srv.start(0).await.unwrap();
    let refused = [post(port, "/api/spend", json!({"initData": ""})).await, post(port, "/api/spend", json!({"initData": signed_by(999)})).await];
    let ok = post(port, "/api/spend", json!({"initData": signed()})).await;
    srv.close().await;
    assert!(refused.iter().all(|r| r.status == 403 && !r.text.contains("today")));
    let body = ok.json();
    assert_eq!(ok.status, 200);
    assert!(approx(body["today"]["usd"].as_f64().unwrap(), 0.25) && body["today"]["unpriced"] == json!(1));
    assert_eq!(body["today"]["by_feature"], json!({"chat": 0.25, "codex": 0.0}));
    assert_eq!(body["days"].as_array().unwrap().len(), 30);
    assert!(body.get("month").is_some() && body["providers"].is_object());
    let mut keys: Vec<&str> = body.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(keys, ["apps_today", "cap", "days", "month", "providers", "today", "yesterday"]);
}

// ---- the cards: lights, music, the TV remote --------------------------------------------------------------------

/// A card's hub, faked: records what reached it (without the initData) and answers what `answer` says.
struct FakeHub {
    bodies: Mutex<Vec<Value>>,
    listing: Vec<Value>,
    answer: Box<dyn Fn(&Value) -> Value + Send + Sync>,
}

#[async_trait::async_trait]
impl eve_link::seams::Hub for FakeHub {
    fn listing(&self) -> Vec<Value> {
        self.listing.clone()
    }
    async fn panel(&self, body: &Value) -> anyhow::Result<Value> {
        let mut b = body.as_object().cloned().unwrap_or_default();
        b.remove("initData");
        self.bodies.lock().push(Value::Object(b));
        Ok((self.answer)(body))
    }
}

/// test_the_lights_card_is_the_owners_only_and_drives_the_lights, the server's half: names and rooms only on
/// /api/me, the card's answers as they are, nothing for anyone else. (The lights' own lines are eve-lights' tests.)
#[tokio::test(flavor = "multi_thread")]
async fn the_lights_card_is_the_owners_only() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let hub = Arc::new(FakeHub {
        bodies: Mutex::default(),
        listing: vec![json!({"name": "floor lamp", "room": "bedroom"}), json!({"name": "strip", "room": "bedroom"})],
        answer: Box::new(|b| match b["action"].as_str() {
            Some("status") => json!({"ok": true, "lights": [{"name": "floor lamp"}, {"name": "strip"}]}),
            _ => json!({"ok": true, "line": "Floor lamp blue. strip: out of range.", "failed": ["strip"]}),
        }),
    });
    let srv = run_server(tmp.path(), 0.0);
    let port = srv.start(0).await.unwrap();
    // no lights lent: none listed
    let off = post(port, "/api/me", json!({"initData": signed()})).await;
    let none = post(port, "/api/lights", json!({"initData": signed(), "action": "status"})).await;
    *srv.lights.lock() = Some(hub.clone());
    let me = post(port, "/api/me", json!({"initData": signed()})).await;
    let stranger = post(port, "/api/lights", json!({"initData": signed_by(999), "action": "set", "color": "red"})).await;
    let status = post(port, "/api/lights", json!({"initData": signed(), "action": "status"})).await;
    let blue = post(port, "/api/lights", json!({"initData": signed(), "action": "set", "target": "all", "color": "blue"})).await;
    srv.close().await;
    assert!(off.json()["lights"] == json!([]) && none.status == 404);
    assert_eq!(none.json(), json!({"ok": false, "line": "eve has no lights on this computer."}));
    assert_eq!(me.json()["lights"], json!([{"name": "floor lamp", "room": "bedroom"}, {"name": "strip", "room": "bedroom"}]));
    assert_eq!(stranger.status, 403);
    let names: Vec<Value> = status.json()["lights"].as_array().unwrap().iter().map(|x| x["name"].clone()).collect();
    assert_eq!(names, [json!("floor lamp"), json!("strip")]);
    assert_eq!(blue.json(), json!({"ok": true, "line": "Floor lamp blue. strip: out of range.", "failed": ["strip"]}));
    assert_eq!(*hub.bodies.lock(), vec![json!({"action": "status"}), json!({"action": "set", "target": "all", "color": "blue"})]);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_music_card_is_the_owners_only_and_drives_spotify() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let hub = Arc::new(FakeHub {
        bodies: Mutex::default(),
        listing: vec![],
        answer: Box::new(|_| json!({"ok": true, "line": "Paused.", "playing": false})),
    });
    let srv = run_server(tmp.path(), 0.0);
    let port = srv.start(0).await.unwrap();
    let off = post(port, "/api/me", json!({"initData": signed()})).await;
    let none = post(port, "/api/spotify", json!({"initData": signed(), "action": "status"})).await;
    *srv.spotify.lock() = Some(hub.clone());
    let me = post(port, "/api/me", json!({"initData": signed()})).await;
    let stranger = post(port, "/api/spotify", json!({"initData": signed_by(999), "action": "pause"})).await;
    let paused = post(port, "/api/spotify", json!({"initData": signed(), "action": "pause"})).await;
    srv.close().await;
    assert!(off.json()["spotify"] == json!(false) && none.status == 404);
    assert_eq!(none.json()["line"], "eve has no Spotify login on this computer.");
    assert_eq!(me.json()["spotify"], json!(true));
    assert_eq!(stranger.status, 403);
    assert_eq!(paused.json(), json!({"ok": true, "line": "Paused.", "playing": false}));
    // the stranger never reached Spotify
    assert_eq!(*hub.bodies.lock(), vec![json!({"action": "pause"})]);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_tv_remote_card_is_the_owners_only_and_drives_the_tv() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let hub = Arc::new(FakeHub {
        bodies: Mutex::default(),
        listing: vec![],
        answer: Box::new(|b| {
            if b["action"] == "status" {
                json!({"ok": true, "power": "on", "app": "YouTube", "volume": 44, "muted": false,
                       "line": "TV on — YouTube, volume 44%", "now": "TV on — YouTube, volume 44%",
                       "apps": ["YouTube", "Netflix"]})
            } else {
                json!({"ok": true, "line": "Pressed up.", "power": "on", "app": "YouTube", "volume": 44,
                       "muted": false, "now": "TV on — YouTube, volume 44%", "apps": ["YouTube", "Netflix"]})
            }
        }),
    });
    let srv = run_server(tmp.path(), 0.0);
    let port = srv.start(0).await.unwrap();
    let off = post(port, "/api/me", json!({"initData": signed()})).await;
    let none = post(port, "/api/firetv", json!({"initData": signed(), "action": "status"})).await;
    *srv.firetv.lock() = Some(hub.clone());
    let me = post(port, "/api/me", json!({"initData": signed()})).await;
    let stranger = post(port, "/api/firetv", json!({"initData": signed_by(999), "action": "power_off"})).await;
    let unsigned = post(port, "/api/firetv", json!({"action": "power_off"})).await;
    let app_tok = post(port, "/api/firetv", json!({"t": "anything", "action": "power_off"})).await;
    let sandboxed =
        raw(port, "POST", "/api/firetv", Some(json!({"initData": signed(), "action": "power_off"})), &[("Origin", "null")]).await;
    let status = post(port, "/api/firetv", json!({"initData": signed(), "action": "status"})).await;
    let up = post(port, "/api/firetv", json!({"initData": signed(), "action": "up"})).await;
    srv.close().await;
    assert!(off.json()["firetv"] == json!(false) && none.status == 404);
    assert_eq!(none.json(), json!({"ok": false, "line": "eve has no TV on this computer."}));
    assert_eq!(me.json()["firetv"], json!(true));
    assert!(stranger.status == 403 && unsigned.status == 403 && app_tok.status == 403);
    // an app page (Origin: null) can't either
    assert_eq!(sandboxed.status, 403);
    assert_eq!(app_tok.json(), json!({"error": "This app can only reach its own data."}));
    assert_eq!(app_tok.header("access-control-allow-origin"), "*");
    assert!(status.json()["now"] == "TV on — YouTube, volume 44%" && status.json()["apps"] == json!(["YouTube", "Netflix"]));
    assert_eq!(up.json()["line"], "Pressed up.");
    // the TV's line is sent as \u escapes, as json.dumps does
    assert!(up.text.contains("TV on \\u2014 YouTube"));
    // only the owner's two calls reached the TV
    assert_eq!(*hub.bodies.lock(), vec![json!({"action": "status"}), json!({"action": "up"})]);
}

// ---- the tunnel and the menu button ---------------------------------------------------------------------------

struct FakeTunnel {
    stopped: AtomicBool,
    exit: Flag,
    port: AtomicI64,
}

#[async_trait::async_trait]
impl Tunnel for FakeTunnel {
    async fn start(&self, port: i64, _timeout: f64) -> anyhow::Result<String> {
        self.port.store(port, Ordering::SeqCst);
        Ok("https://quiet-owl.trycloudflare.com".into())
    }
    async fn wait(&self) -> i64 {
        self.exit.wait().await;
        0
    }
    async fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
    }
}

/// The Bot API as the pins see it: records calls, hands out message ids, can refuse an edit.
#[derive(Default)]
struct FakeBot {
    calls: Mutex<Vec<(String, Obj)>>,
    refuse_edit: bool,
}

impl FakeBot {
    fn bot(self: &Arc<Self>) -> Bot {
        let me = self.clone();
        Arc::new(move |method: String, data: Obj| {
            let me = me.clone();
            async move {
                let n = {
                    let mut calls = me.calls.lock();
                    calls.push((method.clone(), data));
                    calls.len()
                };
                if method == "editMessageText" && me.refuse_edit {
                    return Ok(json!({"ok": false, "description": "Bad Request: message to edit not found"}));
                }
                Ok(json!({"ok": true, "result": {"message_id": 900 + n}}))
            }
            .boxed()
        })
    }
    fn methods(&self) -> Vec<String> {
        self.calls.lock().iter().map(|(m, _)| m.clone()).collect()
    }
    fn sent(&self) -> Vec<Obj> {
        self.calls.lock().iter().filter(|(m, _)| m == "sendMessage").map(|(_, d)| d.clone()).collect()
    }
}

/// A MiniApp with fakes, for what needs no maker (the pins, the URLs): the apps' store and maker are built but unused.
fn plain_app(dir: &Path, bot: &Arc<FakeBot>, factory: Option<TunnelFactory>) -> Arc<MiniApp> {
    let cfg = MiniAppConfig { ledger_path: dir.join("s.json"), ..config(dir, 0.0) };
    MiniApp::new(&cfg, factory, Some(bot.bot()), Some(&dir.join("pins.json")), Some(slow(&["x"]).generate()),
                 Some(&dir.join("apps")), Lent::Given(None), Some(Arc::new(|| "Today is Thursday.".to_string())))
}

fn run_app(dir: &Path, bot: &Arc<FakeBot>) -> (String, Vec<Arc<FakeTunnel>>) {
    let tunnels: Arc<Mutex<Vec<Arc<FakeTunnel>>>> = Arc::default();
    let factory: TunnelFactory = {
        let tunnels = tunnels.clone();
        Arc::new(move || {
            let t = Arc::new(FakeTunnel { stopped: AtomicBool::new(false), exit: Flag::new(), port: AtomicI64::new(0) });
            tunnels.lock().push(t.clone());
            t as Arc<dyn Tunnel>
        })
    };
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let url = rt.block_on(async {
        let app = plain_app(dir, bot, Some(factory));
        let task = {
            let app = app.clone();
            tokio::spawn(async move { app.run().await })
        };
        for _ in 0..200 {
            if bot.methods().iter().any(|m| m == "pinChatMessage" || m == "editMessageText") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let url = app.url.lock().clone();
        app.cancel();
        tokio::time::timeout(Duration::from_secs(10), task).await.expect("run returns on cancel").unwrap();
        url
    });
    let out = tunnels.lock().clone();
    (url, out)
}

#[test]
fn the_menu_stays_the_commands_and_a_pinned_button_opens_the_app() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let bot = Arc::new(FakeBot::default());
    let (url, tunnels) = run_app(tmp.path(), &bot);
    assert!(url == "https://quiet-owl.trycloudflare.com" && tunnels[0].stopped.load(Ordering::SeqCst));
    assert!(tunnels[0].port.load(Ordering::SeqCst) > 0);
    let methods = bot.methods();
    assert_eq!(methods[..3], ["setChatMenuButton", "sendMessage", "pinChatMessage"]);
    let calls = bot.calls.lock().clone();
    // the / menu, kept
    assert_eq!(calls[0].1["menu_button"], json!({"type": "commands"}));
    let button = &calls[1].1["reply_markup"]["inline_keyboard"][0][0];
    assert_eq!(button["web_app"]["url"], json!(format!("{url}/")));
    assert_eq!(button["text"], "Open eve");
    // on stop the pinned message says offline instead of leading nowhere
    assert_eq!(methods.last().unwrap(), "editMessageText");
    assert!(!calls.last().unwrap().1.contains_key("reply_markup"));
    assert_eq!(calls.last().unwrap().1["text"], miniapp::PIN_OFFLINE_TEXT);
    let pins: Value = serde_json::from_str(&std::fs::read_to_string(tmp.path().join("pins.json")).unwrap()).unwrap();
    assert_eq!(pins, json!({OWNER.to_string(): 902}));
}

#[test]
fn the_next_start_edits_the_same_pinned_message() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("pins.json"), json!({OWNER.to_string(): 555}).to_string()).unwrap();
    let bot = Arc::new(FakeBot::default());
    run_app(tmp.path(), &bot);
    let calls = bot.calls.lock().clone();
    let edit = &calls.iter().find(|(m, _)| m == "editMessageText").unwrap().1;
    assert!(edit["message_id"] == json!(555) && edit.contains_key("reply_markup"));
    let methods = bot.methods();
    assert!(!methods.contains(&"sendMessage".to_string()) && !methods.contains(&"pinChatMessage".to_string()));
}

#[test]
fn a_deleted_pinned_message_is_sent_and_pinned_again() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("pins.json"), json!({OWNER.to_string(): 555}).to_string()).unwrap();
    let bot = Arc::new(FakeBot { refuse_edit: true, ..FakeBot::default() });
    run_app(tmp.path(), &bot);
    let methods = bot.methods();
    assert!(methods.contains(&"sendMessage".to_string()) && methods.contains(&"pinChatMessage".to_string()));
    let pins: Value = serde_json::from_str(&std::fs::read_to_string(tmp.path().join("pins.json")).unwrap()).unwrap();
    assert_ne!(pins[OWNER.to_string()], json!(555));
}

#[test]
fn no_cloudflared_is_no_mini_app() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let bot = Arc::new(FakeBot::default());
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let app = plain_app(tmp.path(), &bot, None);
        // with no factory given, one is made only when cloudflared is installed
        if miniapp::cloudflared_bin().is_none() {
            tokio::time::timeout(Duration::from_secs(5), app.run()).await.expect("returns at once");
            assert!(bot.methods().is_empty());
        }
    });
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bot_in_app_only_mode_never_reaches_telegram() {
    let bot = miniapp::bot_caller(TOKEN, Some(&env(&[("EVE_TELEGRAM", " App ")])));
    let reply = bot("sendMessage".into(), Obj::new()).await.unwrap();
    assert_eq!(reply, json!({"ok": false, "description": "Telegram is off (EVE_TELEGRAM=app)"}));
}

#[test]
fn the_open_button_carries_eve_links_update_once_paired() {
    let plain = miniapp::open_button("https://x.trycloudflare.com/", "");
    assert_eq!(Value::Object(plain), json!({"inline_keyboard": [[{"text": "Open eve", "web_app": {"url": "https://x.trycloudflare.com/"}}]]}));
    let paired = miniapp::open_button("https://x/", "https://x/stick#t");
    assert_eq!(paired["inline_keyboard"][1], json!([{"text": eve_stick_link::UPDATE_TEXT, "url": "https://x/stick#t"}]));
}

// ---- the apps routes: build, delete, rename, undo ---------------------------------------------------------------

const DOC: &str = "<!doctype html><html><head><title>Habit Tracker</title><meta name=\"eve-icon\" content=\"✅\"><meta name=\"description\" content=\"Check in daily and keep streaks.\"></head><body>MARK<script>function render() {} eve.load().then(render); function add() { eve.save(state); }</script></body></html>";

fn usage() -> Value {
    json!({"input_tokens": 2000, "output_tokens": 10000})
}

fn page_doc(mark: &str, title: &str) -> String {
    DOC.replace("MARK", mark).replace("Habit Tracker", title)
}

fn doc(mark: &str) -> String {
    page_doc(mark, "Habit Tracker")
}

/// A fake maker Claude that goes through the stages with a pause at each, so the stream can see them. `gate` holds
/// the answer back until the test sets it; `fail_next` makes one call unreachable (FailingClaude); `refuse` declines.
struct SlowClaude {
    answers: Mutex<VecDeque<String>>,
    gate: Option<Arc<Flag>>,
    calls: AtomicI64,
    fail_next: AtomicBool,
    refuse: AtomicBool,
}

fn slow(answers: &[&str]) -> Arc<SlowClaude> {
    slow_owned(answers.iter().map(|a| (*a).to_string()).collect(), None)
}

fn slow_owned(answers: Vec<String>, gate: Option<Arc<Flag>>) -> Arc<SlowClaude> {
    Arc::new(SlowClaude {
        answers: Mutex::new(answers.into()),
        gate,
        calls: AtomicI64::new(0),
        fail_next: AtomicBool::new(false),
        refuse: AtomicBool::new(false),
    })
}

impl SlowClaude {
    fn generate(self: &Arc<Self>) -> Generate {
        let me = self.clone();
        Arc::new(move |_messages: Vec<Value>, progress: MakerProgress| {
            let me = me.clone();
            async move {
                if me.fail_next.swap(false, Ordering::SeqCst) {
                    return Err(anyhow::Error::new(std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "down")));
                }
                if me.refuse.load(Ordering::SeqCst) {
                    return Ok(Generated { text: "I can't help with that.".into(), usage: usage(), stop_reason: "refusal".into(), content: Value::Null });
                }
                me.calls.fetch_add(1, Ordering::SeqCst);
                progress("thinking", 0);
                tokio::time::sleep(Duration::from_millis(50)).await;
                if let Some(gate) = &me.gate {
                    gate.wait().await;
                }
                let text = {
                    let mut answers = me.answers.lock();
                    if answers.len() > 1 { answers.pop_front().unwrap() } else { answers[0].clone() }
                };
                progress("writing", 12_345);
                tokio::time::sleep(Duration::from_millis(50)).await;
                Ok(Generated {
                    text: format!("```html\n{text}\n```"),
                    usage: usage(),
                    stop_reason: "end_turn".into(),
                    content: json!([{"type": "text", "text": text}]),
                })
            }
            .boxed()
        })
    }
}

/// The phone check, faked: a page holding `bad` has one problem; every page gets a picture.
fn phone_check(bad: &str) -> Check {
    let bad = bad.to_string();
    Arc::new(move |html: String, _opts: eve_app_check::CheckOptions| {
        let bad = bad.clone();
        async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let issues: Vec<String> = if html.contains(&bad) { vec!["Tapping Add threw TypeError.".into()] } else { vec![] };
            eve_app_check::CheckReport {
                ok: issues.is_empty(),
                issues,
                taps: 9,
                fields: 2,
                saves: 3,
                screenshot: Some(b"\xff\xd8jpeg".to_vec()),
                ..Default::default()
            }
        }
        .boxed()
    })
}

/// The phone check, faked: every page passes the script and fails one journey.
fn journey_fails() -> Check {
    Arc::new(|_html: String, _opts: eve_app_check::CheckOptions| {
        async move {
            let mut result = eve_jev_verify::JourneyResult::new("Add one", "step");
            result.steps = 2;
            result.step = 1;
            result.step_words = "tap \"Add\"".into();
            result.reason = "tap \"Add\": could not find \"Add\" (the controls on screen: \"Settings\").".into();
            result.evidence = "\"Habit Tracker\"".into();
            let run = eve_jev_verify::JourneyRun { results: vec![result], total: 1, ..Default::default() };
            eve_app_check::CheckReport { ok: false, issues: run.issues(), taps: 9, fields: 2, saves: 3, journeys: Some(run), ..Default::default() }
        }
        .boxed()
    })
}

/// A store call's answer, whichever form the store's port gives it (the value, or a Result of it).
trait Got<T> {
    fn got(self) -> T;
}

macro_rules! plain_got {
    ($($t:ty),*) => { $(impl Got<$t> for $t { fn got(self) -> $t { self } })* };
}

plain_got!(AppInfo, (), Value, i64, Vec<Obj>);

impl<T, E: std::fmt::Debug> Got<T> for Result<T, E> {
    fn got(self) -> T {
        self.expect("the store call")
    }
}

struct Apps {
    app: Arc<MiniApp>,
    bot: Arc<FakeBot>,
}

/// A MiniApp (so the real wiring: AppStore, AppMaker, ledger) with a fake Claude and a fake phone check.
fn apps_server(dir: &Path, claude: Option<Arc<SlowClaude>>, check: Option<Check>, cap: f64) -> Apps {
    let claude = claude.unwrap_or_else(|| slow_owned(vec![doc("v1")], None));
    let bot = Arc::new(FakeBot::default());
    let cfg = config(dir, cap);
    let no_tunnel: TunnelFactory = Arc::new(|| -> Arc<dyn Tunnel> {
        Arc::new(FakeTunnel { stopped: AtomicBool::new(false), exit: Flag::new(), port: AtomicI64::new(0) })
    });
    let app = MiniApp::new(&cfg, Some(no_tunnel), Some(bot.bot()), Some(&dir.join("pins.json")), Some(claude.generate()),
                           Some(&dir.join("apps")), Lent::Given(Some(check.unwrap_or_else(|| phone_check("\0")))),
                           Some(Arc::new(|| "Today is Thursday.".to_string())));
    Apps { app, bot }
}

async fn serving(app: &Arc<MiniApp>) -> i64 {
    app.server.start(0).await.unwrap()
}

async fn make(port: i64, body: Value) -> Vec<Value> {
    let r = post(port, "/api/make", body).await;
    assert!(r.header("content-type").starts_with("text/event-stream"), "{} {}", r.status, r.text);
    events(&r.text)
}

#[tokio::test(flavor = "multi_thread")]
async fn miniapp_wires_the_maker_with_the_ledger_the_check_and_the_context() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let a = apps_server(tmp.path(), None, None, 0.0);
    let made = a.app.maker.make("a habit tracker", None).await;
    assert!(made.ok && made.check.as_ref().unwrap().summary() == "passed: 2 fields filled, 9 taps, 3 saves");
    // tracked, no cap
    assert!(approx(a.app.ledger.spent(), cost_usd("claude-opus-5-5", &usage())));
    assert!(Arc::ptr_eq(&a.app.server.changing, &a.app.changing) && a.app.ledger.cap.is_none());
    assert!(Arc::ptr_eq(&a.app.server.ledger, &a.app.ledger));
    assert!(a.app.server.stick.lock().is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_build_streams_its_stages_then_the_app_the_check_and_todays_spend() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let a = apps_server(tmp.path(), Some(slow_owned(vec![doc("bad"), doc("good")], None)), Some(phone_check("bad")), 0.0);
    let port = serving(&a.app).await;
    let evs = make(port, json!({"initData": signed(), "request": "a habit tracker with streaks"})).await;
    a.app.server.close().await;
    let stages: Vec<String> = evs.iter().filter_map(|e| e.get("s").and_then(Value::as_str)).map(String::from).collect();
    let mut seen: Vec<String> = Vec::new();
    for s in &stages {
        if seen.last() != Some(s) {
            seen.push(s.clone());
        }
    }
    assert_eq!(seen[..4], ["thinking", "writing", "testing", "fixing"]);
    assert!(evs.contains(&json!({"s": "writing", "p": 12_345})) && evs.contains(&json!({"s": "fixing", "p": 1})));
    let done = evs.last().unwrap();
    assert_eq!(done["done"], json!(true));
    assert!(done["url"].as_str().unwrap().starts_with("apps/habit-tracker/?t=") && done["app"]["icon"] == "✅");
    assert!(done["check"] == "passed: 2 fields filled, 9 taps, 3 saves" && done["rounds"] == json!(1) && done["problems"] == json!(0));
    assert!(approx(done["spent_today"].as_f64().unwrap(), 2.0 * cost_usd("claude-opus-5-5", &usage())) && done["cap"].is_null());
    assert!(done["tested"] == json!(true) && done["changed"] == json!(false) && done["issues"] == json!([]));
    assert!(a.app.store.html("habit-tracker").unwrap().contains("good"));
    assert!(a.app.server.builds.lock().is_empty() && a.app.server.building.lock().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_change_goes_to_the_app_named_by_its_slug_and_an_unknown_one_is_refused() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let a = apps_server(tmp.path(), Some(slow_owned(vec![doc("v2")], None)), None, 0.0);
    a.app.store.save(&doc("v1"), "habit tracker", "", None, None).got();
    let port = serving(&a.app).await;
    let changed = make(port, json!({"initData": signed(), "request": "add streaks", "app": "habit-tracker"})).await;
    let missing = post(port, "/api/make", json!({"initData": signed(), "request": "x", "app": "no-such-app"})).await;
    let empty = post(port, "/api/make", json!({"initData": signed(), "request": "   "})).await;
    a.app.server.close().await;
    assert_eq!(changed.last().unwrap()["app"]["versions"], json!(1));
    assert_eq!(changed.last().unwrap()["changed"], json!(true));
    assert!(a.app.store.html("habit-tracker").unwrap().contains("v2"));
    assert!(missing.status == 404 && a.app.changing.lock().is_empty());
    assert_eq!(missing.json(), json!({"error": "There is no such app.", "gone": true}));
    assert!(empty.status == 400 && empty.json()["error"] == "Say what the app should do.");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_build_that_fails_says_why_and_what_today_cost() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let claude = slow(&["x"]);
    claude.refuse.store(true, Ordering::SeqCst);
    let a = apps_server(tmp.path(), Some(claude), None, 0.0);
    let port = serving(&a.app).await;
    let evs = make(port, json!({"initData": signed(), "request": "x"})).await;
    a.app.server.close().await;
    let last = evs.last().unwrap();
    assert!(last["error"].as_str().unwrap().starts_with("I couldn't build it: Claude declined"), "{last}");
    assert!(last["spent_today"].as_f64().unwrap() > 0.0 && last.get("slug").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_build_that_raises_says_what_happened_in_plain_words() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let claude = slow(&["x"]);
    claude.fail_next.store(true, Ordering::SeqCst);
    let a = apps_server(tmp.path(), Some(claude), None, 0.0);
    let port = serving(&a.app).await;
    let evs = make(port, json!({"initData": signed(), "request": "x"})).await;
    a.app.server.close().await;
    let last = evs.last().unwrap();
    assert!(last.get("error").is_some() && last.get("done").is_none(), "{last}");
    assert!(a.app.server.building.lock().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_build_is_refused_only_at_a_cap_the_owner_set() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let capped = apps_server(&tmp.path().join("a"), None, None, 1.0);
    capped.app.ledger.add(1.0);
    let free = apps_server(&tmp.path().join("b"), None, None, 0.0);
    free.app.ledger.add(1000.0);
    let ask = json!({"initData": signed(), "request": "a habit tracker"});
    let port = serving(&capped.app).await;
    let refused = post(port, "/api/make", ask.clone()).await;
    capped.app.server.close().await;
    let port = serving(&free.app).await;
    let evs = make(port, ask).await;
    free.app.server.close().await;
    assert_eq!(refused.status, 429);
    assert_eq!(refused.json()["error"], "Today's Claude budget is used up ($1.00). It resets at midnight.");
    assert_eq!(evs.last().unwrap()["done"], json!(true));
}

/// A POST /api/make on a connection of its own, read as far as its first event: the build is streaming.
async fn start_build(port: i64, body: Value) -> tokio::net::TcpStream {
    let raw_body = body.to_string();
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port as u16)).await.unwrap();
    let head = format!(
        "POST /api/make HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        raw_body.len()
    );
    s.write_all(head.as_bytes()).await.unwrap();
    s.write_all(raw_body.as_bytes()).await.unwrap();
    let mut seen = Vec::new();
    let mut buf = [0u8; 256];
    while !String::from_utf8_lossy(&seen).contains("data: ") {
        let n = tokio::time::timeout(Duration::from_secs(10), s.read(&mut buf)).await.expect("streaming").unwrap();
        assert!(n > 0, "the stream ended early");
        seen.extend_from_slice(&buf[..n]);
    }
    s
}

#[tokio::test(flavor = "multi_thread")]
async fn one_build_at_a_time_per_owner() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let gate = Arc::new(Flag::new());
    let a = apps_server(tmp.path(), Some(slow_owned(vec![doc("v1")], Some(gate.clone()))), None, 0.0);
    let port = serving(&a.app).await;
    let build = tokio::spawn(make(port, json!({"initData": signed(), "request": "a habit tracker"})));
    tokio::time::sleep(Duration::from_millis(200)).await;
    let second = post(port, "/api/make", json!({"initData": signed(), "request": "another"})).await;
    gate.set();
    let build = build.await.unwrap();
    a.app.server.close().await;
    assert_eq!(second.status, 409);
    assert_eq!(second.json()["error"], "Still working on the last one.");
    assert_eq!(build.last().unwrap()["done"], json!(true));
}

#[tokio::test(flavor = "multi_thread")]
async fn rename_undo_and_delete_from_the_page() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let a = apps_server(tmp.path(), None, None, 0.0);
    a.app.store.save(&doc("v1"), "habit tracker", "", None, None).got();
    a.app.store.save(&doc("v2"), "add streaks", "habit-tracker", None, None).got();
    let port = serving(&a.app).await;
    let base = "/api/apps/habit-tracker/";
    let renamed = post(port, &format!("{base}rename"), json!({"initData": signed(), "title": "Daily habits"})).await;
    let undone = post(port, &format!("{base}revert"), json!({"initData": signed()})).await;
    let nothing_left = post(port, &format!("{base}revert"), json!({"initData": signed()})).await;
    let deleted = post(port, &format!("{base}delete"), json!({"initData": signed()})).await;
    let again = post(port, &format!("{base}delete"), json!({"initData": signed()})).await;
    a.app.server.close().await;
    assert_eq!(renamed.json()["app"]["title"], "Daily habits");
    assert!(undone.json()["app"]["versions"] == json!(0) && undone.json()["app"]["title"] == "Daily habits");
    assert!(nothing_left.status == 409 && nothing_left.json()["error"].as_str().unwrap().contains("earlier version"));
    assert!(deleted.json() == json!({"ok": true, "deleted": "Daily habits"}) && again.status == 404);
    assert!(a.app.store.list().is_empty());
    assert_eq!(std::fs::read_dir(tmp.path().join("apps").join(".trash")).unwrap().count(), 1);
}

/// Review D12: /report signed by the owner (not an app token) fell through to the Undo code and reverted.
#[tokio::test(flavor = "multi_thread")]
async fn an_owner_signed_report_never_undoes_the_app() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let a = apps_server(tmp.path(), None, None, 0.0);
    a.app.store.save(&doc("v1"), "habit tracker", "", None, None).got();
    a.app.store.save(&doc("v2"), "add streaks", "habit-tracker", None, None).got();
    let port = serving(&a.app).await;
    let r = post(port, "/api/apps/habit-tracker/report", json!({"initData": signed(), "errors": []})).await;
    a.app.server.close().await;
    assert_eq!(r.json(), json!({"ok": true}));
    // still two versions: nothing undone
    assert_eq!(a.app.store.info("habit-tracker").unwrap().versions, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_empty_name_and_a_missing_app_are_refused() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let a = apps_server(tmp.path(), None, None, 0.0);
    a.app.store.save(&doc("v1"), "habit tracker", "", None, None).got();
    let port = serving(&a.app).await;
    let blank = post(port, "/api/apps/habit-tracker/rename", json!({"initData": signed(), "title": "   "})).await;
    let mut missing = Vec::new();
    for (path, body) in [("/api/apps/nope/rename", json!({"initData": signed(), "title": "x"})),
                         ("/api/apps/nope/revert", json!({"initData": signed()})),
                         ("/api/apps/../delete", json!({"initData": signed()}))] {
        missing.push(post(port, path, body).await.status);
    }
    a.app.server.close().await;
    assert!(blank.status == 400 && blank.json()["error"] == "An app needs a name.");
    assert_eq!(missing, [404, 404, 404]);
    assert_eq!(a.app.store.info("habit-tracker").unwrap().title, "Habit Tracker");
}

#[tokio::test(flavor = "multi_thread")]
async fn only_the_owner_can_delete_rename_or_undo() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let a = apps_server(tmp.path(), None, None, 0.0);
    a.app.store.save(&doc("v1"), "habit tracker", "", None, None).got();
    a.app.store.save(&doc("v2"), "add streaks", "habit-tracker", None, None).got();
    let port = serving(&a.app).await;
    let mut statuses = Vec::new();
    for action in ["delete", "rename", "revert"] {
        for d in [String::new(), signed_by(999), signed().replace("AAH", "AAB")] {
            let path = format!("/api/apps/habit-tracker/{action}");
            statuses.push(post(port, &path, json!({"initData": d, "title": "Mine now"})).await.status);
        }
    }
    a.app.server.close().await;
    assert!(statuses.iter().all(|s| *s == 403), "{statuses:?}");
    let info = a.app.store.info("habit-tracker").unwrap();
    assert!(info.title == "Habit Tracker" && info.versions == 1);
    assert!(a.app.store.html("habit-tracker").unwrap().contains("v2"));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_app_being_changed_cannot_be_deleted_renamed_or_undone_under_the_build() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let gate = Arc::new(Flag::new());
    let a = apps_server(tmp.path(), Some(slow_owned(vec![doc("v3")], Some(gate.clone()))), None, 0.0);
    a.app.store.save(&doc("v1"), "habit tracker", "", None, None).got();
    a.app.store.save(&doc("v2"), "x", "habit-tracker", None, None).got();
    let port = serving(&a.app).await;
    let build = tokio::spawn(make(port, json!({"initData": signed(), "request": "dark mode", "app": "habit-tracker"})));
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut during = Vec::new();
    for action in ["delete", "rename", "revert"] {
        let path = format!("/api/apps/habit-tracker/{action}");
        during.push(post(port, &path, json!({"initData": signed(), "title": "New"})).await.status);
    }
    let busy: Vec<String> = a.app.changing.lock().iter().cloned().collect();
    let twice = post(port, "/api/make", json!({"initData": signed(), "request": "again", "app": "habit-tracker"})).await;
    gate.set();
    build.await.unwrap();
    let after = post(port, "/api/apps/habit-tracker/rename", json!({"initData": signed(), "title": "New"})).await;
    a.app.server.close().await;
    assert!(during == [409, 409, 409] && busy == ["habit-tracker"]);
    // the same owner's second build is the one-at-a-time refusal
    assert_eq!(twice.status, 409);
    assert!(after.status == 200 && a.app.changing.lock().is_empty());
    assert!(a.app.store.html("habit-tracker").unwrap().contains("v3"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_build_from_the_chat_door_also_holds_off_the_page() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let a = apps_server(tmp.path(), None, None, 0.0);
    a.app.store.save(&doc("v1"), "habit tracker", "", None, None).got();
    // what ChatMaker.building does, once the daemon shares it
    a.app.changing.lock().insert("habit-tracker".into());
    let port = serving(&a.app).await;
    let r = post(port, "/api/apps/habit-tracker/delete", json!({"initData": signed()})).await;
    let change = post(port, "/api/make", json!({"initData": signed(), "request": "x", "app": "habit-tracker"})).await;
    let listed = post(port, "/api/apps", json!({"initData": signed()})).await;
    a.app.server.close().await;
    assert!(r.status == 409 && a.app.store.info("habit-tracker").is_some());
    assert!(change.status == 409 && change.json()["error"] == "That app is being changed right now.");
    assert_eq!(listed.json()["building"], json!([{"app": "habit-tracker", "request": "", "stage": "", "secs": null}]));
}

// ---- the home page itself -----------------------------------------------------------------------------------------

fn scripts() -> Vec<String> {
    let page = String::from_utf8(miniapp::PAGE.to_vec()).unwrap();
    let re = regex::Regex::new(r"(?s)<script>(.*?)</script>").unwrap();
    re.captures_iter(&page).map(|c| c[1].to_string()).collect()
}

/// the home page's own script (the one with the apps); "Call eve" has a script block of its own after it
fn page_script() -> String {
    scripts().into_iter().find(|b| b.contains("// ---- apps ----")).expect("the apps script")
}

fn call_script() -> String {
    scripts().into_iter().find(|b| b.contains("new URL(\"api/call\"")).expect("the call script")
}

#[test]
fn the_home_pages_script_parses() {
    let Ok(node) = which::which("node") else {
        eprintln!("skipped: node is not installed");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("page.js"), page_script()).unwrap();
    std::fs::write(tmp.path().join("call.js"), call_script()).unwrap();
    // positive control
    std::fs::write(tmp.path().join("broken.js"), page_script() + "\n})(;").unwrap();
    let check = |name: &str| std::process::Command::new(&node).arg("--check").arg(tmp.path().join(name)).output().unwrap().status.success();
    assert!(check("page.js"));
    assert!(check("call.js"));
    assert!(!check("broken.js"));
}

#[test]
fn app_names_and_descriptions_only_reach_the_page_as_text() {
    // They come from pages Claude wrote. The only innerHTML left is the chat's own Markdown renderer.
    let script = page_script();
    let apps_part = &script[script.find("// ---- apps ----").unwrap()..script.rfind("})();").unwrap()];
    assert!(!apps_part.contains("innerHTML") && !apps_part.contains("insertAdjacentHTML"));
    assert!(apps_part.contains("textContent"));
}

// ---- review fixes: apps fenced in, a bounded shutdown, a build that outlives its page ------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn an_app_page_opens_only_with_its_own_token_and_is_served_sandboxed() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let a = apps_server(tmp.path(), None, None, 0.0);
    a.app.store.save(&doc("SECRET-DEFAULT"), "habit tracker", "", None, None).got();
    a.app.store.save(&page_doc("other", "Reading List"), "reading list", "", None, None).got();
    let port = serving(&a.app).await;
    let good = app_token("habit-tracker", TOKEN, None);
    let old = app_token("habit-tracker", TOKEN, Some(wall() - 2.0 * 86400.0));
    let refused = [
        ("none", get(port, "/apps/habit-tracker/").await),
        ("missing", get(port, "/apps/no-such-app/").await),
        ("other", get(port, &format!("/apps/reading-list/?t={good}")).await),
        ("forged", get(port, &format!("/apps/habit-tracker/?t=9999999999.{}", "0".repeat(40))).await),
        ("old", get(port, &format!("/apps/habit-tracker/?t={old}")).await),
    ];
    let page = get(port, &format!("/apps/habit-tracker/?t={good}")).await;
    let index = get(port, &format!("/apps/habit-tracker/index.html?t={good}")).await;
    a.app.server.close().await;
    // the same answer, and no page
    for (key, r) in &refused {
        assert!(r.status == 302 && !r.text.contains("SECRET"), "{key}");
    }
    assert_eq!(refused[0].1.header("location"), "/?open=habit-tracker");
    assert_eq!(refused[1].1.header("location"), "/?open=no-such-app");
    let csp = page.header("content-security-policy");
    assert!(page.status == 200 && page.text.contains("SECRET-DEFAULT"));
    assert!(csp.starts_with("sandbox allow-scripts") && !csp.contains("allow-same-origin"));
    assert!(csp.contains(&format!("connect-src http://127.0.0.1:{port}/api/apps/habit-tracker/ ")), "{csp}");
    assert_eq!(page.header("referrer-policy"), "no-referrer");
    assert_eq!(page.header("x-content-type-options"), "nosniff");
    assert_eq!(index.status, 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_app_token_reaches_its_own_data_and_nothing_else() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let a = apps_server(tmp.path(), None, None, 0.0);
    a.app.store.save(&doc("v1"), "habit tracker", "", None, None).got();
    a.app.store.save(&page_doc("x", "Reading List"), "reading list", "", None, None).got();
    a.app.store.save_data("reading-list", &json!({"v": 1, "books": ["private"]})).got();
    let port = serving(&a.app).await;
    let t = app_token("habit-tracker", TOKEN, None);
    let mine = "/api/apps/habit-tracker/";
    let save = raw(port, "POST", &format!("{mine}save"), Some(json!({"t": t, "data": {"v": 1}})), &[]).await;
    let load = raw(port, "POST", &format!("{mine}load"), Some(json!({"t": t})), &[]).await;
    let preflight = raw(port, "OPTIONS", &format!("{mine}save"), None, &[]).await;
    let mut fenced = Vec::new();
    for (name, path, extra) in [("other_load", "/api/apps/reading-list/load".to_string(), json!({})),
                                ("other_save", "/api/apps/reading-list/save".to_string(), json!({"data": 1})),
                                ("list", "/api/apps".to_string(), json!({})),
                                ("make", "/api/make".to_string(), json!({"request": "x"})),
                                ("delete", format!("{mine}delete"), json!({}))] {
        let mut body = extra.as_object().unwrap().clone();
        body.insert("t".into(), json!(t));
        fenced.push((name, raw(port, "POST", &path, Some(Value::Object(body)), &[]).await.status));
    }
    // the owner's initData from a sandboxed page (Origin: null) or another site opens nothing either
    fenced.push(("null_origin", raw(port, "POST", "/api/apps", Some(json!({"initData": signed()})), &[("Origin", "null")]).await.status));
    *a.app.server.public_url.lock() = "https://quiet-owl.trycloudflare.com".into();
    fenced.push(("other_site", raw(port, "POST", "/api/apps", Some(json!({"initData": signed()})), &[("Origin", "https://evil.example")]).await.status));
    let home = raw(port, "POST", "/api/apps", Some(json!({"initData": signed()})), &[("Origin", "https://quiet-owl.trycloudflare.com")]).await;
    a.app.server.close().await;
    assert!(save.json()["ok"] == json!(true) && save.json()["bytes"].as_i64().unwrap() > 0);
    assert_eq!(load.json(), json!({"data": {"v": 1}}));
    assert_eq!(load.header("access-control-allow-origin"), "*");
    assert_eq!(preflight.status, 204);
    assert_eq!(preflight.header("access-control-allow-methods"), "POST");
    for (key, status) in fenced {
        assert_eq!(status, 403, "{key}");
    }
    assert_eq!(home.status, 200);
    let listed = home.json();
    assert_eq!(listed["apps"].as_array().unwrap().len(), 2);
    assert!(listed["apps"][0]["open"].as_str().unwrap().starts_with("apps/"));
    assert_eq!(a.app.store.load_data("reading-list").got(), json!({"v": 1, "books": ["private"]}));
    assert!(a.app.store.info("habit-tracker").is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_does_not_wait_for_a_build_that_is_still_streaming() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let gate = Arc::new(Flag::new());
    let a = apps_server(tmp.path(), Some(slow_owned(vec![doc("v1")], Some(gate.clone()))), None, 0.0);
    let port = serving(&a.app).await;
    let stream = start_build(port, json!({"initData": signed(), "request": "a habit tracker"})).await;
    let t0 = Instant::now();
    tokio::time::timeout(Duration::from_secs(20), a.app.server.close()).await.unwrap();
    let took = t0.elapsed().as_secs_f64();
    drop(stream);
    assert!(took < miniapp::CLOSE_WAIT_SECS + 1.0, "{took}");
    // the build's books are closed with it
    assert!(a.app.server.building.lock().is_empty() && a.app.server.builds.lock().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_build_whose_page_went_away_sends_its_result_to_the_chat() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let gate = Arc::new(Flag::new());
    let a = apps_server(tmp.path(), Some(slow_owned(vec![doc("v1")], Some(gate.clone()))), None, 0.0);
    let port = serving(&a.app).await;
    let stream = start_build(port, json!({"initData": signed(), "request": "a habit tracker"})).await;
    let building = post(port, "/api/apps", json!({"initData": signed()})).await.json()["building"].clone();
    let again = post(port, "/api/make", json!({"initData": signed(), "request": "another"})).await;
    // the phone locked
    stream.set_zero_linger().unwrap();
    drop(stream);
    tokio::time::sleep(Duration::from_millis(100)).await;
    gate.set();
    for _ in 0..300 {
        if !a.bot.sent().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    a.app.server.close().await;
    assert!(building[0]["request"] == "a habit tracker" && building[0]["app"] == "", "{building}");
    assert!(building[0]["secs"].is_i64() && building[0]["stage"].is_string());
    assert!(again.status == 409 && again.json()["building"].as_array().is_some_and(|b| !b.is_empty()));
    let sent = a.bot.sent();
    assert!(!sent.is_empty(), "nothing reached the chat");
    assert_eq!(sent[0]["chat_id"], json!(OWNER));
    assert!(sent[0]["text"].as_str().unwrap().starts_with("✅ Habit Tracker is ready."), "{}", sent[0]["text"]);
    assert!(a.app.store.info("habit-tracker").is_some() && a.app.server.builds.lock().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_undo_sent_twice_pops_one_version_only() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let a = apps_server(tmp.path(), None, None, 0.0);
    a.app.store.save(&doc("v1"), "habit tracker", "", None, None).got();
    a.app.store.save(&doc("v2"), "x", "habit-tracker", None, None).got();
    a.app.store.save(&doc("v3"), "y", "habit-tracker", None, None).got();
    let port = serving(&a.app).await;
    let base = "/api/apps/habit-tracker/revert";
    let first = post(port, base, json!({"initData": signed(), "versions": 2})).await;
    let second = post(port, base, json!({"initData": signed(), "versions": 2})).await;
    a.app.server.close().await;
    assert!(first.status == 200 && second.status == 409 && second.json()["error"].as_str().unwrap().contains("already"));
    assert!(a.app.store.html("habit-tracker").unwrap().contains("v2"));
    assert_eq!(a.app.store.info("habit-tracker").unwrap().versions, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_deleted_app_is_restored_from_the_page() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let a = apps_server(tmp.path(), None, None, 0.0);
    a.app.store.save(&doc("v1"), "habit tracker", "", None, None).got();
    a.app.store.save_data("habit-tracker", &json!({"v": 1, "n": 5})).got();
    let port = serving(&a.app).await;
    let base = "/api/apps/habit-tracker/";
    let deleted = post(port, &format!("{base}delete"), json!({"initData": signed()})).await;
    let restored = post(port, &format!("{base}restore"), json!({"initData": signed()})).await;
    let again = post(port, &format!("{base}restore"), json!({"initData": signed()})).await;
    a.app.server.close().await;
    assert!(deleted.status == 200 && restored.json()["app"]["slug"] == "habit-tracker");
    assert_eq!(again.status, 404);
    assert_eq!(a.app.store.load_data("habit-tracker").got(), json!({"v": 1, "n": 5}));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restore_under_a_taken_address_is_refused() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let a = apps_server(tmp.path(), None, None, 0.0);
    a.app.store.save(&doc("v1"), "habit tracker", "", None, None).got();
    let port = serving(&a.app).await;
    let deleted = post(port, "/api/apps/habit-tracker/delete", json!({"initData": signed()})).await;
    a.app.store.save(&doc("new"), "habit tracker", "", None, None).got();
    let taken = post(port, "/api/apps/habit-tracker/restore", json!({"initData": signed()})).await;
    a.app.server.close().await;
    assert_eq!(deleted.status, 200);
    assert_eq!(taken.status, 409, "{}", taken.text);
    assert!(taken.json()["error"].as_str().unwrap().starts_with("A newer app has that address now"));
}

#[tokio::test(flavor = "multi_thread")]
async fn each_spend_mark_crossed_is_a_line_in_the_chat() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let a = apps_server(tmp.path(), None, None, 0.0);
    a.app.ledger.add(6.0);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let sent = a.bot.sent();
    assert_eq!(sent.len(), 1);
    let text = sent[0]["text"].as_str().unwrap();
    assert!(text.contains("$6.00 today") && text.contains("Nothing is capped"), "{text}");
    assert_eq!(text, "Claude has cost $6.00 today (chat and app builds), past $5. Nothing is capped; this is only so you know.");
}

#[test]
fn an_open_button_goes_through_the_home_page() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let bot = Arc::new(FakeBot::default());
    let app = plain_app(tmp.path(), &bot, None);
    assert_eq!(app.app_url("habit-tracker"), "");
    *app.url.lock() = "https://quiet-owl.trycloudflare.com".into();
    assert_eq!(app.app_url("habit-tracker"), "https://quiet-owl.trycloudflare.com/?open=habit-tracker");
    assert_eq!(app.chat_url("habit-tracker"), "https://quiet-owl.trycloudflare.com/?chat=habit-tracker");
}

fn log_buffer() -> (Arc<Mutex<Vec<u8>>>, impl tracing::Subscriber + Send + Sync) {
    let buf: Arc<Mutex<Vec<u8>>> = Arc::default();
    let writer = {
        let buf = buf.clone();
        move || LogWriter(buf.clone())
    };
    let sub = tracing_subscriber::fmt().with_writer(writer).with_ansi(false).with_max_level(tracing::Level::INFO).finish();
    (buf, sub)
}

struct LogWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogWriter {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn log_lines(buf: &Arc<Mutex<Vec<u8>>>) -> Vec<String> {
    String::from_utf8_lossy(&buf.lock()).lines().map(String::from).collect()
}

#[test]
fn an_app_reports_what_it_does_on_the_phone_to_the_log() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let (buf, sub) = log_buffer();
    let _log = tracing::subscriber::set_default(sub);
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let (ok, forged, other) = rt.block_on(async {
        let a = apps_server(tmp.path(), None, None, 0.0);
        a.app.store.save(&doc("v1"), "habit tracker", "", None, None).got();
        let port = serving(&a.app).await;
        let t = app_token("habit-tracker", TOKEN, None);
        let mine = "/api/apps/habit-tracker/report";
        let ok = post(port, mine, json!({"t": t, "platform": "ios", "version": "9.1", "bridge": "proxy",
            "main_button": "shown \"New group\"", "errors": vec!["TypeError: x is null\nsecond line @12"; 7]})).await;
        let forged = post(port, mine, json!({"t": "nope", "platform": "ios"})).await;
        let other = post(port, "/api/apps/reading-list/report", json!({"t": t, "platform": "ios"})).await;
        a.app.server.close().await;
        (ok.status, forged.status, other.status)
    });
    assert!(ok == 200 && forged == 403 && other == 403);
    let lines: Vec<String> = log_lines(&buf).into_iter().filter(|l| l.contains("on the phone")).collect();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("platform=ios") && lines[0].contains("bridge=proxy") && lines[0].contains("errors=7"));
    // five errors at most, one line
    assert_eq!(lines[0].matches(" | ").count(), 5);
}

#[test]
fn opening_loading_and_saving_an_app_are_logged() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let (buf, sub) = log_buffer();
    let _log = tracing::subscriber::set_default(sub);
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let page = rt.block_on(async {
        let a = apps_server(tmp.path(), None, None, 0.0);
        a.app.store.save(&doc("v1"), "habit tracker", "", None, None).got();
        let port = serving(&a.app).await;
        let t = app_token("habit-tracker", TOKEN, None);
        let page = get(port, &format!("/apps/habit-tracker/?t={t}")).await;
        post(port, "/api/apps/habit-tracker/save", json!({"t": t, "data": {"v": 1}})).await;
        post(port, "/api/apps/habit-tracker/load", json!({"t": t})).await;
        let eve_js = get(port, "/eve.js").await;
        a.app.server.close().await;
        assert!(eve_js.status == 200 && eve_js.header("content-type").starts_with("application/javascript"));
        page
    });
    let said = log_lines(&buf).join(" / ");
    assert!(page.status == 200 && page.text.contains("/eve.js"));
    assert!(said.contains("habit-tracker opened") && said.contains("habit-tracker saved") && said.contains("habit-tracker loaded (has data)"), "{said}");
}

// ---- each app's change chat: its history, the Change buttons ------------------------------------------------------

/// A MiniApp with Habit Tracker built, changed, undone and changed again (a failed try in between).
async fn changed_app(dir: &Path) -> Apps {
    let claude = slow_owned(vec![doc("v1"), doc("v2"), doc("v3")], None);
    let a = apps_server(dir, Some(claude.clone()), None, 0.0);
    a.app.maker.make("a habit tracker", None).await;
    a.app.maker.edit("habit-tracker", "make the buttons bigger", None).await;
    a.app.store.revert("habit-tracker").got();
    claude.fail_next.store(true, Ordering::SeqCst);
    a.app.maker.edit("habit-tracker", "add categories", None).await;
    a.app.maker.edit("habit-tracker", "add categories", None).await;
    a
}

#[tokio::test(flavor = "multi_thread")]
async fn history_is_the_apps_thread_and_only_the_owner_gets_it() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let a = changed_app(tmp.path()).await;
    let port = serving(&a.app).await;
    let url = "/api/apps/habit-tracker/history";
    let t = app_token("habit-tracker", TOKEN, None);
    let mine = post(port, url, json!({"initData": signed()})).await;
    let refused = [
        post(port, url, json!({"initData": signed_by(999)})).await.status,
        post(port, url, json!({})).await.status,
        post(port, url, json!({"t": t})).await.status,
        raw(port, "POST", url, Some(json!({"initData": signed()})), &[("Origin", "null")]).await.status,
    ];
    let missing = post(port, "/api/apps/no-such-app/history", json!({"initData": signed()})).await;
    let bad_slug = post(port, "/api/apps/..%2Fx/history", json!({"initData": signed()})).await;
    a.app.server.close().await;
    assert_eq!(refused, [403; 4]);
    assert!(missing.status == 404 && missing.json()["gone"] == json!(true) && bad_slug.status == 404);
    let j = mine.json();
    assert!(j["app"]["slug"] == "habit-tracker" && j["app"]["open"].as_str().unwrap().starts_with("apps/habit-tracker/?t="));
    assert!(j["building"].is_null());
    assert!(approx(j["spent_today"].as_f64().unwrap(), 3.0 * cost_usd("claude-opus-5-5", &usage())));
    let kinds: Vec<(String, String)> = j["thread"].as_array().unwrap().iter()
        .map(|i| (i["kind"].as_str().unwrap().to_string(), i["text"].as_str().unwrap().to_string())).collect();
    let want: Vec<(String, String)> = [("request", "a habit tracker"), ("request", "make the buttons bigger"), ("undo", "(undo)"),
                                       ("request", "add categories"), ("request", "add categories")]
        .iter().map(|(k, t)| ((*k).to_string(), (*t).to_string())).collect();
    assert_eq!(kinds, want);
    let results: Vec<&Value> = j["thread"].as_array().unwrap().iter().map(|i| &i["result"]).collect();
    let (made, bigger, undo, failed, cats) = (results[0], results[1], results[2], results[3], results[4]);
    assert!(made["ok"] == json!(true) && made["version"] == json!(1) && made["check"] == "passed: 2 fields filled, 9 taps, 3 saves");
    assert!(bigger["version"] == json!(2) && undo.is_null() && cats["version"] == json!(3) && cats["ok"] == json!(true));
    assert!((made["usd"].as_f64().unwrap() - cost_usd("claude-opus-5-5", &usage())).abs() <= 1e-4 && made["tested"] == json!(true));
    assert!(failed["ok"] == json!(false) && failed["reason"] == "the build stopped before it finished", "{failed}");
    assert_eq!(failed["usd"].as_f64(), Some(0.0));
}

const STAGE_NAMES: [&str; 5] = ["thinking", "writing", "testing", "fixing", "saving"];

#[tokio::test(flavor = "multi_thread")]
async fn history_shows_the_change_running_on_the_app_now() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let gate = Arc::new(Flag::new());
    let a = apps_server(tmp.path(), Some(slow_owned(vec![doc("v2")], Some(gate.clone()))), None, 0.0);
    a.app.store.save(&doc("v1"), "habit tracker", "", None, None).got();
    a.app.store.save(&page_doc("v1", "Reading List"), "reading list", "", None, None).got();
    let port = serving(&a.app).await;
    let mut stream = start_build(port, json!({"initData": signed(), "request": "add streaks", "app": "habit-tracker"})).await;
    let mine = post(port, "/api/apps/habit-tracker/history", json!({"initData": signed()})).await.json()["building"].clone();
    let other = post(port, "/api/apps/reading-list/history", json!({"initData": signed()})).await.json()["building"].clone();
    // a change from the chat door
    a.app.changing.lock().insert("reading-list".into());
    let door = post(port, "/api/apps/reading-list/history", json!({"initData": signed()})).await.json()["building"].clone();
    a.app.changing.lock().remove("reading-list");
    gate.set();
    let mut rest = Vec::new();
    tokio::time::timeout(Duration::from_secs(20), stream.read_to_end(&mut rest)).await.unwrap().unwrap();
    drop(stream);
    let after = post(port, "/api/apps/habit-tracker/history", json!({"initData": signed()})).await.json();
    a.app.server.close().await;
    assert!(mine["app"] == "habit-tracker" && mine["request"] == "add streaks");
    assert!(STAGE_NAMES.contains(&mine["stage"].as_str().unwrap()));
    assert!(mine["secs"].is_i64() && other.is_null());
    assert_eq!(door, json!({"app": "reading-list", "request": "", "stage": "", "secs": null}));
    let thread = after["thread"].as_array().unwrap();
    assert!(after["building"].is_null() && thread.last().unwrap()["text"] == "add streaks");
    assert_eq!(thread.last().unwrap()["result"]["version"], json!(2));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_page_build_finished_in_the_chat_has_a_change_button_after_a_change() {
    for changed in [true, false] {
        let _spend = SpendDir::new();
        let tmp = tempfile::tempdir().unwrap();
        let a = apps_server(tmp.path(), None, None, 0.0);
        *a.app.url.lock() = "https://quiet-owl.trycloudflare.com".into();
        let out = json!({"done": true, "changed": changed, "app": {"slug": "habit-tracker", "title": "Habit Tracker", "icon": "✅"}});
        a.app.build_result(OWNER, out.as_object().unwrap()).await;
        let sent = a.bot.sent();
        let rows = &sent[0]["reply_markup"]["inline_keyboard"];
        let mut want = vec![json!([{"text": "Open Habit Tracker", "web_app": {"url": "https://quiet-owl.trycloudflare.com/?open=habit-tracker"}}])];
        if changed {
            want.push(json!([{"text": "Change", "web_app": {"url": "https://quiet-owl.trycloudflare.com/?chat=habit-tracker"}}]));
        }
        assert_eq!(*rows, Value::Array(want));
        assert_eq!(sent[0]["text"], json!(format!("✅ Habit Tracker is {}.", if changed { "updated" } else { "ready" })));
        assert!(a.app.chat_url("habit-tracker").ends_with("/?chat=habit-tracker"));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_build_finished_with_a_problem_names_it_in_the_chat() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let a = apps_server(tmp.path(), None, None, 0.0);
    let out = json!({"done": true, "changed": false, "issues": ["Tapping Add threw TypeError."],
                     "app": {"slug": "habit-tracker", "title": "", "icon": ""}});
    a.app.build_result(OWNER, out.as_object().unwrap()).await;
    let sent = a.bot.sent();
    // no tunnel: no buttons
    assert_eq!(sent[0]["text"], "Your app is ready.\nIt may still have a problem: Tapping Add threw TypeError.");
    assert!(!sent[0].contains_key("reply_markup"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_change_from_the_page_that_failed_after_the_page_went_away_still_offers_its_change_chat() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let a = apps_server(tmp.path(), None, None, 0.0);
    *a.app.url.lock() = "https://quiet-owl.trycloudflare.com".into();
    a.app.store.save(&doc("v1"), "habit tracker", "", None, None).got();
    let first = json!({"error": "I couldn't change it: Claude declined to build that.", "changed": true, "slug": "habit-tracker"});
    a.app.build_result(OWNER, first.as_object().unwrap()).await;
    let second = json!({"error": "I couldn't build it: no.", "changed": false});
    a.app.build_result(OWNER, second.as_object().unwrap()).await;
    let sent = a.bot.sent();
    assert_eq!(sent[0]["reply_markup"]["inline_keyboard"],
               json!([[{"text": "Change", "web_app": {"url": "https://quiet-owl.trycloudflare.com/?chat=habit-tracker"}}]]));
    assert!(!sent[1].contains_key("reply_markup"));
    assert_eq!(sent[1]["text"], "I couldn't build it: no.");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_page_hears_a_journey_problem_in_plain_words_and_a_failed_change_as_a_change() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let claude = slow_owned(vec![doc("v1")], None);
    let a = apps_server(tmp.path(), Some(claude.clone()), Some(journey_fails()), 0.0);
    let port = serving(&a.app).await;
    let done = make(port, json!({"initData": signed(), "request": "a habit tracker"})).await.pop().unwrap();
    assert_eq!(done["issues"], json!(["Couldn't tap \"Add\" in \"Add one\"."]), "{done}");
    claude.refuse.store(true, Ordering::SeqCst);
    let failed = make(port, json!({"initData": signed(), "request": "x", "app": "habit-tracker"})).await.pop().unwrap();
    a.app.server.close().await;
    assert!(failed["error"] == "I couldn't change it: Claude declined to build that." && failed["slug"] == "habit-tracker", "{failed}");
}

#[tokio::test(flavor = "multi_thread")]
async fn eve_links_stick_link_points_the_pins_and_needs_the_tunnel() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let a = apps_server(tmp.path(), None, None, 0.0);
    // no tunnel: no link, nothing sent
    assert_eq!(a.app.stick_link(false).await, "");
    assert!(a.bot.methods().is_empty());
    *a.app.url.lock() = "https://quiet-owl.trycloudflare.com".into();
    let link = a.app.stick_link(false).await;
    assert!(link.starts_with("https://quiet-owl.trycloudflare.com"), "{link}");
    assert_eq!(link, a.app.stick_url());
    let calls = a.bot.calls.lock().clone();
    let sent = &calls.iter().find(|(m, _)| m == "sendMessage").unwrap().1;
    assert_eq!(sent["reply_markup"]["inline_keyboard"][1][0]["url"], json!(link));
    assert!(tmp.path().join("stick-link.json").exists());
    let rotated = a.app.stick_link(true).await;
    assert!(!rotated.is_empty() && rotated != link);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_eve_link_doors_are_closed_until_they_are_lent() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let srv = run_server(tmp.path(), 0.0);
    let port = srv.start(0).await.unwrap();
    let stick_page = get(port, "/stick").await;
    let call = get(port, "/api/call").await;
    let stick = get(port, "/api/stick").await;
    let link = get(port, "/api/link/apps").await;
    srv.close().await;
    assert!(stick_page.status == 200 && stick_page.bytes == eve_stick_link::PAIR_PAGE.as_bytes());
    for (k, v) in eve_stick_link::PAIR_HEADERS {
        assert_eq!(stick_page.header(&k.to_lowercase()), v, "{k}");
    }
    assert!(call.status == 404 && stick.status == 404 && link.status == 404);
}

#[test]
fn quick_tunnel_without_cloudflared_gives_no_address() {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let tunnel = miniapp::QuickTunnel::new("/nonexistent/cloudflared");
        assert!(tunnel.start(1, 1.0).await.is_err());
        // a tunnel that never started is stopped and waited at once
        tunnel.stop().await;
        assert_eq!(tunnel.wait().await, -1);
    });
}

#[cfg(unix)]
#[test]
fn quick_tunnel_reads_its_address_and_stops_cloudflared() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = tmp.path().join("cloudflared");
    std::fs::write(&fake, "#!/bin/sh\necho 'INF |  https://brave-cat-lamp-owl.trycloudflare.com   |' >&2\nexec sleep 30\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let tunnel = miniapp::QuickTunnel::new(fake.to_str().unwrap());
        let url = tunnel.start(8080, 10.0).await.unwrap();
        assert_eq!(url, "https://brave-cat-lamp-owl.trycloudflare.com");
        assert_eq!(*tunnel.url.lock(), url);
        let t0 = Instant::now();
        tunnel.stop().await;
        assert!(t0.elapsed() < Duration::from_secs(4));
        // SIGTERM
        assert_eq!(tunnel.wait().await, -15);
    });
    // the silent one times out
    std::fs::write(&fake, "#!/bin/sh\nexec sleep 30\n").unwrap();
    rt.block_on(async {
        let tunnel = miniapp::QuickTunnel::new(fake.to_str().unwrap());
        let t0 = Instant::now();
        assert!(tunnel.start(8080, 0.5).await.is_err());
        assert!(t0.elapsed() < Duration::from_secs(4));
        // stopped when it gave no address
        assert_eq!(tunnel.wait().await, -15);
    });
}

// ---- the lights card over the real lights (test_the_lights_card_is_the_owners_only_and_drives_the_lights) ------

type LightsApplied = Arc<Mutex<Vec<(String, eve_lights::Change)>>>;

/// The card's `Driver`: the strip is out of range; every other light records its change.
struct CardDriver {
    name: String,
    applied: LightsApplied,
}

#[async_trait::async_trait]
impl eve_lights::Driver for CardDriver {
    async fn state(&self) -> Result<eve_lights::State, eve_lights::LightError> {
        Ok(eve_lights::State { on: Some(true), brightness: Some(30), rgb: Some([0, 255, 0]), kelvin: None })
    }
    async fn apply(&self, change: &eve_lights::Change) -> Result<(), eve_lights::LightError> {
        if self.name == "strip" {
            return Err(eve_lights::LightError::Connection("out of range".into()));
        }
        self.applied.lock().push((self.name.clone(), change.clone()));
        Ok(())
    }
    async fn close(&self) -> Result<(), eve_lights::LightError> {
        Ok(())
    }
}

/// `lights.Lights` as the card's hub (what the daemon lends the Mini App).
struct LightsCard(eve_lights::Lights);

#[async_trait::async_trait]
impl eve_link::seams::Hub for LightsCard {
    fn listing(&self) -> Vec<Value> {
        self.0.listing()
    }
    async fn panel(&self, body: &Value) -> anyhow::Result<Value> {
        Ok(self.0.panel(body).await)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_the_lights_card_is_the_owners_only_and_drives_the_lights() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let applied: LightsApplied = Arc::default();
    let drivers: HashMap<String, eve_lights::DriverFactory> = eve_lights::KINDS
        .iter()
        .map(|k| {
            let a = applied.clone();
            let f: eve_lights::DriverFactory = Arc::new(move |lt: &eve_lights::Light| {
                Arc::new(CardDriver { name: lt.name.clone(), applied: a.clone() }) as Arc<dyn eve_lights::Driver>
            });
            (k.to_string(), f)
        })
        .collect();
    let mut lamp = eve_lights::Light::new("floor lamp", "govee");
    lamp.room = "bedroom".into();
    lamp.ip = "192.0.2.1".into();
    lamp.device = "d".into();
    let mut strip = eve_lights::Light::new("strip", "triones");
    strip.room = "bedroom".into();
    strip.address = "u".into();
    let hub = Arc::new(LightsCard(eve_lights::Lights::with(vec![lamp, strip], Some(drivers), Duration::from_secs_f64(eve_lights::LIGHT_TIMEOUT_SECS))));
    let srv = run_server(tmp.path(), 0.0);
    let port = srv.start(0).await.unwrap();
    let off = post(port, "/api/me", json!({"initData": signed()})).await; // no lights lent: none listed
    let none = post(port, "/api/lights", json!({"initData": signed(), "action": "status"})).await;
    *srv.lights.lock() = Some(hub);
    let me = post(port, "/api/me", json!({"initData": signed()})).await;
    let stranger = post(port, "/api/lights", json!({"initData": signed_by(999), "action": "set", "color": "red"})).await;
    let status = post(port, "/api/lights", json!({"initData": signed(), "action": "status"})).await;
    let blue = post(port, "/api/lights", json!({"initData": signed(), "action": "set", "target": "all", "color": "blue"})).await;
    let dim = post(port, "/api/lights", json!({"initData": signed(), "action": "set", "target": "floor lamp", "brightness": 40})).await;
    let bad = post(port, "/api/lights", json!({"initData": signed(), "action": "set", "color": "plaid"})).await;
    srv.close().await;
    assert!(off.json()["lights"] == json!([]) && none.status == 404);
    assert_eq!(me.json()["lights"], json!([{"name": "floor lamp", "room": "bedroom"}, {"name": "strip", "room": "bedroom"}]));
    assert!(!me.text.contains("192.0.2.1")); // names and rooms only, never addresses
    let red = eve_lights::Change { color: eve_lights::parse_color("red"), ..Default::default() };
    assert!(stranger.status == 403 && applied.lock().first().map(|(n, c)| (n.clone(), c.clone())) != Some(("floor lamp".to_string(), red)));
    let names: Vec<Value> = status.json()["lights"].as_array().unwrap().iter().map(|x| x["name"].clone()).collect();
    assert_eq!(names, [json!("floor lamp"), json!("strip")]);
    assert_eq!(blue.json(), json!({"ok": true, "line": "Floor lamp blue. strip: out of range.", "failed": ["strip"]}));
    assert_eq!(dim.json()["line"], "Floor lamp at 40%.");
    assert_eq!(
        *applied.lock(),
        vec![
            ("floor lamp".to_string(), eve_lights::Change { color: eve_lights::parse_color("blue"), ..Default::default() }),
            ("floor lamp".to_string(), eve_lights::Change { brightness: Some(40), ..Default::default() }),
        ]
    );
    assert_eq!(bad.json(), json!({"ok": false, "line": "Unknown colour 'plaid'."}));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_each_spend_mark_crossed_is_a_line_in_the_chat_and_nothing_is_refused() {
    let _spend = SpendDir::new();
    let tmp = tempfile::tempdir().unwrap();
    let led = SpendLedger::new(&tmp.path().join("s.json"), None, Some(Arc::new(|| "2026-09-24".to_string())));
    let marks: Arc<Mutex<Vec<(f64, f64)>>> = Arc::default();
    {
        let marks = marks.clone();
        *led.on_cross.lock() = Some(Arc::new(move |mark, total| marks.lock().push((mark, total))));
    }
    led.add(4.0);
    led.add(2.0); // past 5
    led.add(1.0);
    led.add(60.0); // past 20 and 50 at once: one line, the higher mark
    assert!(*marks.lock() == vec![(5.0, 6.0), (50.0, 67.0)] && !led.over());
    // the Mini App wires the mark to a line in the owner's chat
    let apps = apps_server(&tmp.path().join("b"), None, None, 0.0);
    apps.app.ledger.add(6.0);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let sent = apps.bot.sent();
    assert_eq!(sent.len(), 1, "{sent:?}");
    let text = sent[0]["text"].as_str().unwrap();
    assert!(text.contains("$6.00 today") && text.contains("Nothing is capped"), "{text}");
}
