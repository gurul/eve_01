//! Unit `apps_store`: apps_maker.py 1-1129 (apps_maker/mod); tests: test_apps_maker.py pieces, the prompt, the store (60-327), the server's app routes (587-641), review fixes (773-1051), eve.js (1259-1349).
//!
//! The shared fakes are in `support` (architect-owned); this unit's own go here: Script (a fake Claude),
//! FakeCheck (the phone check), maker_for, the chat door's Chat and FakeMiniApp, and a small browser harness for
//! eve.js over app_check's CDP client.
//!
//! The tests that drive AppMaker, ChatMaker, serve_app_html or the Mini App server run code of the `apps_maker`
//! and `miniapp` units; they are ignored here until those units are merged into this crate, then un-ignored.

mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use futures::FutureExt;
use parking_lot::Mutex;
use regex::Regex;
use serde_json::{json, Map, Value};
use tempfile::TempDir;

use eve_app_check::cdp::{find_chromium, Browser, ContextOptions, Page};
use eve_app_check::{CheckOptions, CheckReport};
use eve_brain::apps_maker::{
    data_sample, extract_html, load_issues, owner_context, serve_app_html, slugify, static_issues, AppMaker, AppStore,
    ChatMaker, Check, Generate, Generated, MakerProgress, PageDisk, StoreError, DATA_SAMPLE_CHARS, EVE_JS,
    MAKER_PROMPT, MAX_VERSIONS, SKELETON,
};
use eve_brain::miniapp::{cost_usd, MiniAppConfig, MiniAppServer, SpendLedger, DEFAULT_MAKE_EFFORT, DEFAULT_MODEL};
use eve_brain::prelude::{Environ, MakerHost, SayFn, SendButtons, SendPhoto, SpawnFn};

type Obj = Map<String, Value>;

const TOKEN: &str = "123456:TEST-token";
const OWNER: i64 = 4242;
const DOC: &str = "<!doctype html><html><head><title>Habit Tracker</title><meta name=\"eve-icon\" content=\"✅\">\
<meta content=\"Check in daily and keep streaks.\" name=\"description\"></head><body>hi<script>\
eve.load().then(render); function add() { eve.save(state); }</script></body></html>";

fn usage() -> Value {
    json!({"input_tokens": 2000, "output_tokens": 10000})
}

fn doc(marker: &str) -> String {
    DOC.replace("hi", marker)
}

fn doc_titled(marker: &str, title: &str) -> String {
    DOC.replace("hi", marker).replace("Habit Tracker", title)
}

fn fenced(html: &str) -> String {
    format!("Here it is.\n```html\n{html}\n```")
}

fn tmp() -> TempDir {
    tempfile::tempdir().expect("tempdir")
}

fn missing<T: std::fmt::Debug>(r: Result<T, StoreError>) -> bool {
    matches!(r, Err(StoreError::Missing(_)))
}

fn invalid<T: std::fmt::Debug>(r: Result<T, StoreError>) -> bool {
    matches!(r, Err(StoreError::Invalid(_)))
}

/// tests/initdata.py `sign_init_data`: an initData string signed the way Telegram signs it.
fn sign_init_data(fields: &[(&str, String)], token: &str) -> String {
    use hmac::{Hmac, Mac};
    type HmacSha256 = Hmac<sha2::Sha256>;
    let mut sorted: Vec<&(&str, String)> = fields.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(b.0));
    let check = sorted.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("\n");
    let mut mac = HmacSha256::new_from_slice(b"WebAppData").expect("hmac");
    mac.update(token.as_bytes());
    let secret = mac.finalize().into_bytes();
    let mut mac = HmacSha256::new_from_slice(&secret).expect("hmac");
    mac.update(check.as_bytes());
    let hash = hex::encode(mac.finalize().into_bytes());
    let mut out = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in fields {
        out.append_pair(k, v);
    }
    out.append_pair("hash", &hash);
    out.finish()
}

fn signed(user_id: i64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("clock").as_secs();
    sign_init_data(&[("auth_date", now.to_string()), ("user", json!({"id": user_id}).to_string().replace(':', ": "))], TOKEN)
}

// ---- pieces -------------------------------------------------------------------------------------------------------

#[test]
fn test_slugs_are_short_and_safe() {
    assert_eq!(slugify("Habit Tracker!"), "habit-tracker");
    assert_eq!(slugify("../../etc"), "etc");
    assert_eq!(slugify(""), "app");
}

#[test]
fn test_only_a_complete_document_counts() {
    let cases = [
        (format!("Here you go:\n```html\n{DOC}\n```\nEnjoy"), true),
        (DOC.to_string(), true),
        ("```html\n<!doctype html><html><head>".to_string(), false), // cut off: no </html>
        ("Sorry, I can't.".to_string(), false),
    ];
    for (text, ok) in cases {
        assert_eq!(extract_html(&text).is_some(), ok, "{text}");
    }
}

#[test]
#[ignore = "needs serve_app_html from the apps_maker unit (not filled in this unit; un-ignore once it merges)"]
fn test_served_apps_get_telegram_and_window_eve_first() {
    let out = serve_app_html(DOC);
    assert!(out.find("src=\"/eve.js\"").expect("eve.js") < out.find("<title>").expect("title"));
    assert!(out.contains("telegram-web-app.js"));
}

#[test]
fn test_static_checks_hold_the_storage_contract_and_the_allowed_hosts() {
    assert_eq!(static_issues(DOC), Vec::<String>::new());
    let bare = "<html><head></head><body><script>render()</script></body></html>";
    let found = static_issues(bare).join(" ");
    assert!(found.contains("<title>") && found.contains("eve.load()") && found.contains("eve.save()"));
    let loads = DOC.replace(
        "hi",
        "<link rel=\"stylesheet\" href=\"https://fonts.googleapis.com/css2?family=Inter\">\
         <script>fetch(\"https://api.example.com/x\"); new XMLHttpRequest().open(\"GET\", \"//evil.test/a\")</script>",
    );
    let host = Regex::new(r"loads from (\S+);").unwrap();
    let hosts: Vec<String> = static_issues(&loads)
        .iter()
        .map(|i| host.captures(i).expect("a host")[1].to_string())
        .collect();
    assert_eq!(hosts, ["api.example.com", "evil.test", "fonts.googleapis.com"]);
    let fine = DOC.replace(
        "hi",
        "<script src=\"https://cdn.jsdelivr.net/npm/chart.js@4.4.1/dist/chart.umd.min.js\"></script>\
         <script src=\"https://telegram.org/js/telegram-web-app.js\"></script>\
         <svg xmlns=\"http://www.w3.org/2000/svg\"></svg><input placeholder=\"https://example.com\">\
         <a href=\"https://example.com\">x</a><script>tg.openLink(\"https://example.com\")</script>",
    );
    assert_eq!(static_issues(&fine), Vec::<String>::new());
}

fn env(pairs: &[(&str, &str)]) -> Environ {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

#[test]
fn test_owner_context_has_the_day_the_time_and_a_units_guess() {
    let now = chrono::DateTime::parse_from_rfc3339("2026-09-24T21:40:00+00:00").unwrap();
    let us = owner_context(
        Some(&env(&[("EVE_TIMEZONE", "America/New_York"), ("EVE_LOCATION", "Portland, OR, USA")])),
        Some(&now),
    );
    assert!(us.contains("Thursday, 24 September 2026 (2026-09-24)") && us.contains("17:40"), "{us}");
    assert!(us.contains("America/New_York"), "{us}");
    assert!(us.contains("Portland") && us.contains("miles") && us.contains("Sunday"), "{us}");
    let uk = owner_context(Some(&env(&[("EVE_TIMEZONE", "Europe/London"), ("EVE_LOCATION", "Leeds, UK")])), Some(&now));
    assert!(uk.contains("22:40") && uk.contains('£') && uk.contains("Monday"), "{uk}");
    let anywhere = owner_context(Some(&env(&[("EVE_TIMEZONE", "Not/AZone")])), Some(&now));
    assert!(anywhere.contains("2026-09-24") && anywhere.contains("No location is configured"), "{anywhere}");
    // the phone's locale formats dates and numbers; a format named here would be hard-coded into the app
    let banned = Regex::new(r"first dates|hour clock|digit grouping").unwrap();
    let pune = owner_context(Some(&env(&[("EVE_LOCATION", "Pune, India")])), Some(&now));
    for ctx in [&us, &uk, &anywhere, &pune] {
        assert!(!banned.is_match(ctx), "{ctx}");
    }
    assert!(pune.contains("Indian rupees"), "{pune}");
}

#[test]
fn test_a_data_sample_keeps_the_shape_and_shortens_the_bulk() {
    let small = json!({"v": 1, "habits": [{"name": "Read"}]});
    assert_eq!(serde_json::from_str::<Value>(&data_sample(&small, DATA_SAMPLE_CHARS as i64)).unwrap(), small);
    let one: Vec<Value> =
        (1..29).map(|i| json!({"day": format!("2026-01-{i:02}"), "note": "x".repeat(300)})).collect();
    let log: Vec<Value> = (0..20).flat_map(|_| one.clone()).collect();
    let big = json!({"v": 2, "log": log});
    let out = data_sample(&big, DATA_SAMPLE_CHARS as i64);
    assert!(out.chars().count() <= DATA_SAMPLE_CHARS + 10 && out.contains("more items") && out.contains("\"v\": 2"));
}

// ---- the prompt ---------------------------------------------------------------------------------------------------

#[test]
fn test_the_prompt_carries_the_contract_the_runtime_and_the_skeleton() {
    for must in [
        "name=\"eve-icon\"",
        "name=\"description\"",
        "<title>",
        "eve.load()",
        "eve.save(value)",
        "cdn.jsdelivr.net",
        "MainButton",
        "BackButton",
        "HapticFeedback",
        "showConfirm",
        "isVersionAtLeast",
        "--tg-safe-area-inset",
        "--tg-content-safe-area-inset",
        "disableVerticalSwipes",
        "themeChanged",
        "crypto.randomUUID",
        "migrate",
        "WHOLE updated file",
    ] {
        assert!(MAKER_PROMPT.contains(must), "{must}");
    }
    assert!(MAKER_PROMPT.contains(SKELETON));
}

#[test]
fn test_the_prompt_never_names_a_storage_or_date_api_it_must_not_pick() {
    // A name in a system prompt is a candidate for the model's output, even inside a "don't": the prompt says
    // what to use, and this proves the names it must not reach for are absent.
    let banned = [
        "localStorage",
        "sessionStorage",
        "indexedDB",
        "toISOString",
        "document.cookie",
        // a locale tag or a length attribute named anywhere is one the model hard-codes into every app
        "en-GB",
        "en-US",
        "en-IN",
        "de-DE",
        "maxlength",
    ];
    let found: Vec<&str> = banned.iter().copied().filter(|b| MAKER_PROMPT.contains(b)).collect();
    assert_eq!(found, Vec::<&str>::new());
    // the checker finds what is there
    assert!(MAKER_PROMPT.contains("eve.save") && MAKER_PROMPT.contains("getFullYear"));
}

#[tokio::test]
async fn test_the_reference_skeleton_itself_passes_the_phone_check() {
    if find_chromium().is_none() {
        return; // pytest.importorskip("playwright.async_api")
    }
    let r = eve_app_check::check_app(SKELETON, CheckOptions::default()).await;
    assert!(r.skipped.is_empty() && r.ok, "{:?}", r.issues);
    assert_eq!(static_issues(SKELETON), Vec::<String>::new());
    assert!(r.saves >= 2 && r.screenshot.is_some());
}

#[tokio::test]
async fn test_the_skeleton_never_saves_over_data_it_cannot_read() {
    if find_chromium().is_none() {
        return;
    }
    let newer = json!({"v": 9, "cups": [{"at": 1}]});
    let r = eve_app_check::check_app(SKELETON, CheckOptions { seed: Some(newer.clone()), ..CheckOptions::default() }).await;
    assert!(r.saved == newer && r.saves == 0, "{:?}", r.saved); // the owner's data, untouched
    assert!(r.issues.iter().any(|i| i.contains("could not read that data")), "{:?}", r.issues);
}

// ---- a browser for eve.js (Playwright's page, over app_check's CDP client) ----------------------------------------

fn phone(width: u32, height: u32, has_touch: bool) -> ContextOptions {
    ContextOptions {
        width,
        height,
        device_scale_factor: 1.0,
        is_mobile: false,
        has_touch,
        locale: "en-US".into(),
        color_scheme: "light".into(),
    }
}

/// `page.route("https://app.test/**", ...)`: every request is answered here, the app's page for an /apps/ URL and
/// "HOME" for anything else.
async fn serve_app_test(page: &Page, app_page: String) {
    let mut events = page.take_events().expect("events");
    page.route_all().await.expect("route");
    let p = page.clone();
    tokio::spawn(async move {
        while let Some(ev) = events.recv().await {
            if ev.method != "Fetch.requestPaused" {
                continue;
            }
            let id = ev.params.get("requestId").and_then(Value::as_str).unwrap_or("").to_string();
            let url = ev.params.pointer("/request/url").and_then(Value::as_str).unwrap_or("").to_string();
            let body = if url.contains("/apps/") { app_page.clone() } else { "HOME".to_string() };
            let _ = p.fulfill(&id, 200, &[("content-type", "text/html")], &body).await;
        }
    });
}

/// `async with page.expect_navigation(): <act>`: the action, then the next page load.
async fn navigation<F: std::future::Future<Output = ()>>(page: &Page, act: F) -> String {
    let before = page.loads();
    act.await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while page.loads() <= before {
        assert!(tokio::time::Instant::now() < deadline, "no navigation");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    page.evaluate("location.href", None).await.expect("href").as_str().unwrap_or("").to_string()
}

/// Playwright's `page.evaluate(script)` for a script of statements (app_check's `evaluate` takes one expression
/// or function): its completion value.
async fn run_js(page: &Page, js: &str) -> Value {
    let r = page
        .call("Runtime.evaluate", json!({"expression": js, "returnByValue": true, "awaitPromise": true}))
        .await
        .expect("evaluate");
    assert!(r.get("exceptionDetails").is_none(), "{js}: {r}");
    r.pointer("/result/value").cloned().unwrap_or(Value::Null)
}

/// `page.evaluate(js)` that starts a navigation: the page may go away before it answers.
async fn evaluate_leaving(page: &Page, js: &str) {
    let _ = page.call("Runtime.evaluate", json!({"expression": js})).await;
}

#[tokio::test]
async fn test_eve_js_sends_one_save_at_a_time_and_the_newest_value_wins() {
    if find_chromium().is_none() {
        return;
    }
    let harness = r#"window.sent = []; window.inFlight = 0; window.most = 0;
    window.fetch = (path, init) => { inFlight++; most = Math.max(most, inFlight);
      sent.push({ path, keepalive: init.keepalive, body: JSON.parse(init.body) });
      return new Promise((done) => setTimeout(() => { inFlight--;
        done({ ok: true, json: async () => ({ ok: true, data: { n: 1 } }) }); }, 120)); }; true"#;
    let b = Browser::launch(&[]).await.expect("chromium");
    let ctx = b.new_context(phone(1280, 720, false)).await.expect("context");
    let page = ctx.new_page().await.expect("page");
    page.set_content("<html><body></body></html>").await.expect("content");
    run_js(&page, harness).await;
    page.evaluate(&format!("(() => {{ const s = document.createElement('script'); s.textContent = {}; \
                            document.head.appendChild(s); return true; }})()", json!(EVE_JS)), None)
        .await
        .expect("eve.js");
    let out = page
        .evaluate(
            r#"async () => {
              const s = { n: 1 };
              const all = [eve.save(s)]; s.n = 2; all.push(eve.save(s)); s.n = 3; all.push(eve.save(s));
              const results = await Promise.all(all);
              const loaded = await eve.load();
              return { results, loaded, sent, most };
            }"#,
            None,
        )
        .await
        .expect("saves");
    b.close().await;
    let saves: Vec<&Value> = out["sent"]
        .as_array()
        .expect("sent")
        .iter()
        .filter(|s| s["path"].as_str().unwrap_or("").ends_with("/save"))
        .collect();
    assert_eq!(out["most"], json!(1)); // never two requests at once
    let data: Vec<&Value> = saves.iter().map(|s| &s["body"]["data"]).collect();
    assert_eq!(data, [&json!({"n": 1}), &json!({"n": 3})]); // the middle one folded into the newest
    assert_eq!(out["results"], json!([true, true, true]));
    assert!(saves.iter().all(|s| s["keepalive"] == json!(true)));
    assert_eq!(out["loaded"], json!({"n": 1}));
    assert!(saves[0]["body"].get("t").is_some() && saves[0]["body"].get("initData").is_none());
}

#[tokio::test]
async fn test_back_on_an_apps_first_view_goes_home_and_elsewhere_runs_the_apps_own_handler() {
    if find_chromium().is_none() {
        return;
    }
    // Telegram's BackButton as its script builds it: one native button, handlers kept by Telegram.
    let stub = r#"window.Telegram = { WebApp: { isVersionAtLeast: () => true, BackButton: (() => {
      const cbs = []; const b = { isVisible: false,
        show() { b.isVisible = true; return b; }, hide() { b.isVisible = false; return b; },
        onClick(f) { cbs.push(f); return b; }, offClick(f) { const i = cbs.indexOf(f); if (i >= 0) cbs.splice(i, 1); return b; },
        press() { cbs.slice().forEach((f) => f()); } }; return b; })() } };"#;
    let app = r#"<script>const bb = Telegram.WebApp.BackButton; window.backs = 0;
      bb.onClick(() => { window.backs++; bb.hide(); });   // the app's own "back to my home view"
      bb.hide();                                           // it starts on its home view</script>"#;
    let page_html = format!("<html><head><script>{stub}</script><script>{EVE_JS}</script></head><body>{app}</body></html>");
    let b = Browser::launch(&[]).await.expect("chromium");
    let ctx = b.new_context(phone(1280, 720, false)).await.expect("context");
    let page = ctx.new_page().await.expect("page");
    serve_app_test(&page, page_html).await;
    page.goto("https://app.test/apps/x/?t=abc", 30_000).await.expect("goto");
    let shown_on_home = page.evaluate("Telegram.WebApp.BackButton.isVisible", None).await.expect("visible");
    run_js(&page, "Telegram.WebApp.BackButton.show(); Telegram.WebApp.BackButton.press()").await;
    let inner = page.evaluate("({ backs: window.backs, url: location.href, slug: eve.slug })", None).await.expect("inner");
    let url = navigation(&page, evaluate_leaving(&page, "Telegram.WebApp.BackButton.press()")).await;
    b.close().await;
    assert_eq!(shown_on_home, json!(true)); // Back shows on the app's first view too
    // the app's own back
    assert_eq!(inner, json!({"backs": 1, "url": "https://app.test/apps/x/?t=abc", "slug": "x"}));
    assert_eq!(url, "https://app.test/"); // then home, to eve's list
}

// ---- the store ----------------------------------------------------------------------------------------------------

#[test]
fn test_store_saves_lists_finds_and_describes() {
    let t = tmp();
    let store = AppStore::new(t.path());
    let a = store.save(DOC, "a habit tracker", "", None, None).unwrap();
    assert_eq!(
        (a.slug.as_str(), a.title.as_str(), a.icon.as_str(), a.description.as_str(), a.versions),
        ("habit-tracker", "Habit Tracker", "✅", "Check in daily and keep streaks.", 0)
    );
    let b = store.save(DOC, "another", "", None, None).unwrap(); // same title: a second app, not an overwrite
    assert_eq!(b.slug, "habit-tracker-2");
    let edited = store.save(&doc("v2"), "add streaks", &a.slug, None, None).unwrap();
    assert!(edited.slug == a.slug && edited.created == a.created && edited.versions == 1);
    assert!(store.html(&a.slug).unwrap().contains("v2"));
    let texts: Vec<String> = store.requests(&a.slug).iter().map(|r| r["text"].clone()).collect();
    assert_eq!(texts, ["a habit tracker", "add streaks"]);
    assert!(store.find("my habit tracker").unwrap().slug.starts_with("habit-tracker"));
    assert!(store.find("zebra").is_none());
    assert_eq!(store.find("Habit Tracker").unwrap().slug, "habit-tracker");
    let mut slugs: Vec<String> = store.list().into_iter().map(|x| x.slug).collect();
    slugs.sort();
    assert_eq!(slugs, ["habit-tracker", "habit-tracker-2"]);
    let mut keys: Vec<String> = a.as_dict().keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, ["created", "description", "icon", "slug", "title", "undo_warning", "updated", "versions"]);
}

#[test]
fn test_versions_are_capped_and_undo_pops_the_newest() {
    let t = tmp();
    let store = AppStore::new(t.path());
    let slug = store.save(&doc("v0"), "first", "", None, None).unwrap().slug;
    for i in 1..MAX_VERSIONS + 3 {
        store.save(&doc(&format!("v{i}")), &format!("change {i}"), &slug, None, None).unwrap();
    }
    assert_eq!(store.info(&slug).unwrap().versions, MAX_VERSIONS as i64);
    let back = store.revert(&slug).unwrap();
    assert!(store.html(&slug).unwrap().contains(&format!("v{}", MAX_VERSIONS + 1)));
    assert_eq!(back.versions, MAX_VERSIONS as i64 - 1);
    for _ in 0..MAX_VERSIONS - 1 {
        store.revert(&slug).unwrap();
    }
    assert!(store.html(&slug).unwrap().contains("v2")); // v0 and v1 fell off the end
    assert!(missing(store.revert(&slug)));
}

#[test]
fn test_undo_restores_the_old_title_and_icon_unless_renamed() {
    let t = tmp();
    let store = AppStore::new(t.path());
    let slug = store.save(DOC, "x", "", None, None).unwrap().slug;
    store.save(&doc_titled("v2", "Streaks").replace('✅', "🔥"), "rename via code", &slug, None, None).unwrap();
    let now = store.info(&slug).unwrap();
    assert_eq!((now.title.as_str(), now.icon.as_str()), ("Streaks", "🔥"));
    let back = store.revert(&slug).unwrap();
    assert_eq!((back.title.as_str(), back.icon.as_str()), ("Habit Tracker", "✅"));
}

#[test]
fn test_a_renamed_app_keeps_its_name_through_changes() {
    let t = tmp();
    let store = AppStore::new(t.path());
    let slug = store.save(DOC, "x", "", None, None).unwrap().slug;
    assert_eq!(store.rename(&slug, "  Daily   habits ").unwrap().title, "Daily habits");
    store.save(&doc_titled("v2", "Something Else"), "change", &slug, None, None).unwrap();
    let info = store.info(&slug).unwrap();
    assert!(info.title == "Daily habits" && info.slug == slug);
    assert!(invalid(store.rename(&slug, "   ")));
    assert!(missing(store.rename("no-such-app", "x")));
}

#[test]
fn test_delete_moves_the_app_to_the_trash() {
    let t = tmp();
    let store = AppStore::new(t.path());
    let slug = store.save(DOC, "x", "", None, None).unwrap().slug;
    store.save_data(&slug, &json!({"v": 1})).unwrap();
    store.delete(&slug).unwrap();
    assert!(store.list().is_empty() && store.find("habit tracker").is_none() && store.info(&slug).is_none());
    let trashed: Vec<PathBuf> = std::fs::read_dir(t.path().join(".trash")).unwrap().map(|e| e.unwrap().path()).collect();
    assert_eq!(trashed.len(), 1);
    assert!(trashed[0].file_name().unwrap().to_str().unwrap().starts_with(&slug));
    assert!(trashed[0].join("data.json").exists());
    assert!(missing(store.load_data(&slug)));
    assert!(missing(store.delete(&slug)));
    let again = store.save(DOC, "x", "", None, None).unwrap(); // the name is free again
    assert_eq!(again.slug, slug);
    assert_eq!(store.load_data(&slug).unwrap(), Value::Null);
}

#[test]
fn test_apps_from_before_versions_keep_their_previous_page_and_request() {
    let t = tmp();
    let d = t.path().join("old-app");
    std::fs::create_dir(&d).unwrap();
    std::fs::write(d.join("index.html"), doc("now")).unwrap();
    std::fs::write(d.join("index.prev.html"), doc("before")).unwrap();
    std::fs::write(
        d.join("meta.json"),
        json!({"title": "Old App", "created": "2026-09-24T10:00:00", "updated": "2026-09-24T11:00:00",
               "request": "an old app"})
        .to_string(),
    )
    .unwrap();
    let store = AppStore::new(t.path());
    assert_eq!(store.info("old-app").unwrap().versions, 1);
    assert_eq!(store.requests("old-app")[0]["text"], "an old app");
    store.revert("old-app").unwrap();
    assert!(store.html("old-app").unwrap().contains("before") && !d.join("index.prev.html").exists());
}

#[test]
fn test_no_path_leaves_the_apps_folder() {
    let long = "a".repeat(49);
    for bad in ["../secrets", "a/b", "Habit", "habit\n", ".trash", "", "-x", long.as_str(), "..", "/etc"] {
        let t = tmp();
        let store = AppStore::new(&t.path().join("apps"));
        std::fs::create_dir(t.path().join("apps")).unwrap();
        std::fs::create_dir(t.path().join("secrets")).unwrap();
        std::fs::write(t.path().join("secrets").join("index.html"), "secret").unwrap();
        assert!(store.info(bad).is_none() && store.html(bad).is_none() && store.requests(bad).is_empty(), "{bad:?}");
        assert!(missing(store.load_data(bad)), "{bad:?}");
        assert!(missing(store.revert(bad)), "{bad:?}");
        assert!(missing(store.delete(bad)), "{bad:?}");
        assert!(missing(store.save_data(bad, &json!(1))), "{bad:?}");
        assert!(missing(store.rename(bad, "x")), "{bad:?}");
        if !bad.is_empty() {
            // "" means a new app, named by its title
            assert!(invalid(store.save(DOC, "x", bad, None, None)), "{bad:?}");
        }
        assert_eq!(std::fs::read_to_string(t.path().join("secrets").join("index.html")).unwrap(), "secret");
    }
}

#[test]
fn test_a_symlinked_app_folder_is_refused() {
    let t = tmp();
    let outside = t.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("index.html"), DOC).unwrap();
    std::fs::write(outside.join("meta.json"), json!({"title": "x", "created": "a", "updated": "b"}).to_string()).unwrap();
    std::fs::create_dir(t.path().join("apps")).unwrap();
    std::os::unix::fs::symlink(&outside, t.path().join("apps").join("sneaky")).unwrap();
    let store = AppStore::new(&t.path().join("apps"));
    assert!(store.info("sneaky").is_none() && store.list().is_empty());
    assert!(missing(store.delete("sneaky")));
    assert!(outside.join("index.html").exists());
}

#[test]
fn test_data_round_trips_and_is_bounded() {
    let t = tmp();
    let store = AppStore::new(t.path());
    let slug = store.save(DOC, "x", "", None, None).unwrap().slug;
    assert_eq!(store.load_data(&slug).unwrap(), Value::Null);
    store.save_data(&slug, &json!({"habits": ["run"], "done": {"2026-09-24": ["run"]}})).unwrap();
    assert_eq!(store.load_data(&slug).unwrap()["habits"], json!(["run"]));
    assert!(invalid(store.save_data(&slug, &json!("x".repeat(1_100_000)))));
    assert!(missing(store.load_data("no-such-app")));
}

// ---- the maker and the chat door, faked (as test_apps_maker.py fakes them) ----------------------------------------

/// One scripted answer of the fake Claude.
#[derive(Clone)]
enum Answer {
    Text(String),
    /// Claude could not be reached (anthropic.APIConnectionError)
    Unreachable,
}

/// A fake Claude: answers from a list, one per call, and records every call's messages.
#[derive(Clone)]
struct Script {
    answers: Arc<Mutex<Vec<Answer>>>,
    calls: Arc<Mutex<Vec<Vec<Value>>>>,
}

impl Script {
    fn new(answers: Vec<Answer>) -> Self {
        Self { answers: Arc::new(Mutex::new(answers)), calls: Arc::new(Mutex::new(Vec::new())) }
    }

    fn texts(texts: &[String]) -> Self {
        Self::new(texts.iter().cloned().map(Answer::Text).collect())
    }

    fn generate(&self) -> Generate {
        let me = self.clone();
        Arc::new(move |messages: Vec<Value>, progress: MakerProgress| {
            let me = me.clone();
            async move {
                let n = {
                    let mut calls = me.calls.lock();
                    calls.push(messages);
                    calls.len()
                };
                let a = {
                    let mut answers = me.answers.lock();
                    if answers.len() > 1 { answers.remove(0) } else { answers[0].clone() }
                };
                let text = match a {
                    Answer::Text(t) => t,
                    Answer::Unreachable => {
                        // a real refused connection: what the SDK raises as APIConnectionError
                        let e = reqwest::Client::new().get("http://127.0.0.1:9/").send().await.expect_err("refused");
                        return Err(eve_openrouter::Error::Http(e).into());
                    }
                };
                progress("thinking", 0);
                progress("writing", text.chars().count() as i64);
                Ok(Generated {
                    usage: usage(),
                    stop_reason: "end_turn".into(),
                    content: json!([{"type": "text", "text": text, "n": n}]),
                    text,
                })
            }
            .boxed()
        })
    }
}

/// The phone check, faked: the issues for a page are keyed by a marker in it. `skip` names a marker whose page
/// the check could not run on.
#[derive(Clone, Default)]
struct FakeCheck {
    issues: Vec<(String, Vec<String>)>,
    skip: Option<String>,
    seeds: Arc<Mutex<Vec<Option<Value>>>>,
}

impl FakeCheck {
    fn new(issues: &[(&str, &[&str])]) -> Self {
        Self {
            issues: issues.iter().map(|(k, v)| (k.to_string(), v.iter().map(|s| s.to_string()).collect())).collect(),
            ..Self::default()
        }
    }

    fn check(&self) -> Check {
        let me = self.clone();
        Arc::new(move |html: String, opts: CheckOptions| {
            let me = me.clone();
            async move {
                if me.skip.as_ref().is_some_and(|m| html.contains(m.as_str())) {
                    return CheckReport { skipped: "the check could not run (Error)".into(), ..CheckReport::default() };
                }
                me.seeds.lock().push(opts.seed);
                let found = me.issues.iter().find(|(k, _)| html.contains(k.as_str())).map(|(_, v)| v.clone()).unwrap_or_default();
                CheckReport {
                    ok: found.is_empty(),
                    issues: found,
                    taps: 3,
                    fields: 1,
                    saves: 2,
                    screenshot: Some(b"\xff\xd8jpeg".to_vec()),
                    ..CheckReport::default()
                }
            }
            .boxed()
        })
    }
}

fn maker_for(root: &Path, generate: Generate, check: Option<Check>) -> (Arc<AppMaker>, Arc<SpendLedger>, Arc<AppStore>) {
    let ledger = Arc::new(SpendLedger::new(&root.join("spend.json"), Some(0.0), None));
    let store = Arc::new(AppStore::new(&root.join("apps")));
    let l = ledger.clone();
    let maker = AppMaker::new(
        store.clone(),
        generate,
        Arc::new(|u: &Value| cost_usd("claude-opus-5-5", u)),
        Arc::new(move |usd: f64| l.add(usd)),
        Some(Arc::new(|| "Today is Thursday.".to_string())),
        Some(check.unwrap_or_else(|| FakeCheck::default().check())),
        2,
    );
    (maker, ledger, store)
}

const URL: &str = "https://quiet-owl.trycloudflare.com";

struct FakeMiniApp {
    maker: Arc<AppMaker>,
    store: Arc<AppStore>,
    url: String,
}

#[async_trait::async_trait]
impl MakerHost for FakeMiniApp {
    fn url(&self) -> String {
        self.url.clone()
    }
    fn maker(&self) -> Arc<AppMaker> {
        self.maker.clone()
    }
    fn store(&self) -> Arc<AppStore> {
        self.store.clone()
    }
    fn app_url(&self, slug: &str) -> String {
        if self.url.is_empty() { String::new() } else { format!("{}/apps/{slug}/", self.url) }
    }
    fn chat_url(&self, slug: &str) -> String {
        if self.url.is_empty() { String::new() } else { format!("{}/?chat={slug}", self.url) }
    }
    async fn stick_link(&self, _rotate: bool) -> String {
        String::new()
    }
}

type Sent = (i64, String, Vec<(String, String)>);
type Photo = (i64, Vec<u8>, String, Vec<(String, String)>);

#[derive(Clone, Default)]
struct Chat {
    sent: Arc<Mutex<Vec<Sent>>>,
    photos: Arc<Mutex<Vec<Photo>>>,
    said: Arc<Mutex<Vec<String>>>,
}

impl Chat {
    fn send(&self) -> SendButtons {
        let sent = self.sent.clone();
        Arc::new(move |chat, text, buttons| {
            sent.lock().push((chat, text, buttons));
            async { Ok(()) }.boxed()
        })
    }
    fn say(&self) -> SayFn {
        let said = self.said.clone();
        Arc::new(move |_chat, text| {
            said.lock().push(text);
            async {}.boxed()
        })
    }
    fn send_photo(&self) -> SendPhoto {
        let photos = self.photos.clone();
        Arc::new(move |chat, jpeg, caption, buttons| {
            photos.lock().push((chat, jpeg, caption, buttons));
            async { Ok(()) }.boxed()
        })
    }
}

/// The spawned builds of a door, awaited as `asyncio.gather(*spawned)`.
#[derive(Clone, Default)]
struct Spawned(Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>);

impl Spawned {
    fn spawn_fn(&self) -> SpawnFn {
        let jobs = self.0.clone();
        Arc::new(move |fut: BoxFuture<'static, ()>, _name: &str| jobs.lock().push(tokio::spawn(fut)))
    }
    async fn gather(&self) {
        let jobs: Vec<_> = self.0.lock().drain(..).collect();
        for j in jobs {
            j.await.expect("build");
        }
    }
}

fn door_for(root: &Path, script: &Script, check: Option<Check>) -> (Arc<ChatMaker>, Arc<AppStore>, Spawned) {
    let (maker, _, store) = maker_for(root, script.generate(), check);
    let spawned = Spawned::default();
    let door = ChatMaker::new(Arc::new(FakeMiniApp { maker, store: store.clone(), url: URL.into() }), spawned.spawn_fn());
    (door, store, spawned)
}

fn open_button() -> (String, String) {
    ("Open Habit Tracker".into(), format!("{URL}/apps/habit-tracker/"))
}

fn change_button() -> (String, String) {
    ("Change".into(), format!("{URL}/?chat=habit-tracker"))
}

fn args(v: Value) -> Obj {
    v.as_object().cloned().expect("args")
}

// ---- the server's app routes (the /api/make stream is miniapp's, tested in test_miniapp.py) -----------------------

fn server_for(root: &Path, script: &Script) -> (Arc<MiniAppServer>, Arc<AppStore>) {
    let cfg = MiniAppConfig {
        enabled: true,
        token: TOKEN.into(),
        owner_ids: vec![OWNER],
        model: DEFAULT_MODEL.into(),
        daily_usd: 0.0,
        make_effort: DEFAULT_MAKE_EFFORT.into(),
        ledger_path: root.join("spend.json"),
    };
    let (maker, ledger, store) = maker_for(root, script.generate(), None);
    (MiniAppServer::new(&cfg, ledger, Some(b"home"), Some(store.clone()), Some(maker), None, None), store)
}

async fn call(port: i64, method: &str, path: &str, body: Option<Value>) -> reqwest::Response {
    let c = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let url = format!("http://127.0.0.1:{port}{path}");
    let r = if method == "GET" { c.get(url) } else { c.post(url).json(&body.unwrap_or_else(|| json!({}))) };
    r.send().await.expect("response")
}

#[tokio::test]
#[ignore = "needs the miniapp and apps_maker units (not filled in this unit; un-ignore once they merge)"]
async fn test_an_app_is_served_with_window_eve_and_keeps_its_data() {
    let t = tmp();
    let (srv, store) = server_for(t.path(), &Script::texts(&[fenced(DOC)]));
    store.save(DOC, "habit tracker", "", None, None).unwrap();
    let port = srv.start(0).await;
    let listed: Value = call(port, "POST", "/api/apps", Some(json!({"initData": signed(OWNER)}))).await.json().await.unwrap();
    let page = call(port, "GET", &format!("/{}", listed["apps"][0]["open"].as_str().unwrap()), None).await;
    let page_status = page.status().as_u16();
    let page_text = page.text().await.unwrap();
    let js = call(port, "GET", "/eve.js", None).await.text().await.unwrap();
    let saved: Value = call(
        port,
        "POST",
        "/api/apps/habit-tracker/save",
        Some(json!({"initData": signed(OWNER), "data": {"habits": ["read"]}})),
    )
    .await
    .json()
    .await
    .unwrap();
    let loaded: Value = call(port, "POST", "/api/apps/habit-tracker/load", Some(json!({"initData": signed(OWNER)})))
        .await
        .json()
        .await
        .unwrap();
    let missing_app = call(port, "GET", "/apps/nope/", None).await;
    srv.close().await;
    let slugs: Vec<&str> = listed["apps"].as_array().unwrap().iter().map(|a| a["slug"].as_str().unwrap()).collect();
    assert_eq!(slugs, ["habit-tracker"]);
    assert!(page_status == 200 && page_text.contains("/eve.js") && js.contains("window.eve"));
    assert!(saved["ok"].as_bool().unwrap() && loaded["data"] == json!({"habits": ["read"]}));
    assert_eq!(missing_app.status().as_u16(), 302);
    assert_eq!(missing_app.headers()["location"], "/?open=nope");
}

#[tokio::test]
#[ignore = "needs the miniapp and apps_maker units (not filled in this unit; un-ignore once they merge)"]
async fn test_the_app_api_is_owner_only() {
    let t = tmp();
    let gen = Script::texts(&[fenced(DOC)]);
    let (srv, store) = server_for(t.path(), &gen);
    store.save(DOC, "x", "", None, None).unwrap();
    let port = srv.start(0).await;
    let mut statuses = Vec::new();
    for p in ["/api/make", "/api/apps", "/api/apps/habit-tracker/load", "/api/apps/habit-tracker/save"] {
        for d in [String::new(), signed(999)] {
            let r = call(port, "POST", p, Some(json!({"initData": d, "request": "x", "data": 1}))).await;
            statuses.push(r.status().as_u16());
        }
    }
    let data = store.load_data("habit-tracker").unwrap();
    srv.close().await;
    assert!(statuses.iter().all(|s| *s == 403), "{statuses:?}");
    assert!(gen.calls.lock().is_empty() && data.is_null());
}

// ---- review fixes: the right app, a safe save, no guesses, a hard gate on outside code ----------------------------

#[test]
fn test_a_slug_names_its_own_app_before_any_title() {
    // Two builds of "Notes" are notes and notes-2, both titled Notes: "notes" is the first, never the newest.
    let t = tmp();
    let store = AppStore::new(t.path());
    let old = store.save(&doc_titled("old", "Notes"), "notes", "", None, None).unwrap().slug;
    std::thread::sleep(Duration::from_millis(1100)); // the second one is newer
    let new = store.save(&doc_titled("new", "Notes"), "notes again", "", None, None).unwrap().slug;
    assert_eq!((old.as_str(), new.as_str()), ("notes", "notes-2"));
    assert_eq!(store.list()[0].slug, "notes-2");
    assert_eq!(store.find("notes").unwrap().slug, "notes");
    assert_eq!(store.find("notes-2").unwrap().slug, "notes-2");
    store.rename("notes-2", "habit-tracker").unwrap(); // a title that is another app's slug
    store.save(DOC, "habit tracker", "", None, None).unwrap();
    assert_eq!(store.find("habit-tracker").unwrap().slug, "habit-tracker");
}

#[tokio::test]
#[ignore = "needs AppMaker from the apps_maker unit (not filled in this unit; un-ignore once it merges)"]
async fn test_changing_the_older_of_two_same_named_apps_changes_that_one() {
    let t = tmp();
    let gen = Script::texts(&[fenced(&doc_titled("changed", "Notes"))]);
    let (maker, _, store) = maker_for(t.path(), gen.generate(), None);
    store.save(&doc_titled("old", "Notes"), "notes", "", None, None).unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    store.save(&doc_titled("new", "Notes"), "notes again", "", None, None).unwrap();
    let made = maker.edit("notes", "bigger text", None).await; // the slug the Mini App sends
    assert!(made.ok && made.app.as_ref().unwrap().slug == "notes");
    assert!(store.html("notes").unwrap().contains("changed") && store.html("notes-2").unwrap().contains("new"));
    let first = gen.calls.lock()[0][0]["content"].as_str().unwrap_or("").to_string();
    assert!(first.contains("old")); // Claude was shown the right file
}

/// A disk that fills up on the new page: its write or its move into place.
struct FullDisk {
    fails: &'static str,
}

impl PageDisk for FullDisk {
    fn write(&self, path: &Path, text: &str) -> std::io::Result<()> {
        if self.fails == "write" && path.file_name().is_some_and(|n| n == "index.tmp") {
            return Err(std::io::Error::from_raw_os_error(28)); // No space left on device
        }
        std::fs::write(path, text)
    }
    fn replace(&self, from: &Path, to: &Path) -> std::io::Result<()> {
        if self.fails == "replace" && from.file_name().is_some_and(|n| n == "index.tmp") {
            return Err(std::io::Error::from_raw_os_error(28));
        }
        std::fs::rename(from, to)
    }
}

#[test]
fn test_a_failed_page_write_leaves_the_app_as_it_was() {
    for fails in ["write", "replace"] {
        let t = tmp();
        let store = AppStore::new(t.path());
        let slug = store.save(&doc("v1"), "first", "", None, None).unwrap().slug;
        store.save_data(&slug, &json!({"v": 1})).unwrap();
        let r = store.save_with(&doc("v2"), "change", &slug, None, None, &FullDisk { fails });
        assert!(matches!(r, Err(StoreError::Io(_))), "{fails}");
        let info = store.info(&slug);
        // still listed, still opens
        assert!(info.is_some_and(|i| i.versions == 0) && store.html(&slug).unwrap().contains("v1"), "{fails}");
        let listed: Vec<String> = store.list().into_iter().map(|a| a.slug).collect();
        assert_eq!(listed.as_slice(), std::slice::from_ref(&slug));
        assert_eq!(store.load_data(&slug).unwrap(), json!({"v": 1}));
        assert!(!t.path().join(&slug).join("index.tmp").exists(), "{fails}");
    }
}

#[tokio::test]
#[ignore = "needs ChatMaker from the apps_maker unit (not filled in this unit; un-ignore once it merges)"]
async fn test_the_chat_door_never_deletes_undoes_or_renames_on_a_guess() {
    let t = tmp();
    let chat = Chat::default();
    let (door, store, _) = door_for(t.path(), &Script::texts(&[fenced(DOC)]), None);
    store.save(&doc_titled("a", "Water Log"), "a", "", None, None).unwrap();
    store.save(&doc_titled("b", "Water Plants"), "b", "", None, None).unwrap();
    store.save(&doc_titled("c", "Habit Tracker"), "c", "", None, None).unwrap();
    store.save(&doc_titled("c2", "Habit Tracker"), "c", "habit-tracker", None, None).unwrap();
    let mut out: std::collections::HashMap<&str, Obj> = std::collections::HashMap::new();
    for (name, tool, a) in [
        ("both_water", "delete_app", json!({"app": "the water app"})),
        ("no_such", "delete_app", json!({"app": "water tracker"})),
        ("undo_no_such", "undo_app", json!({"app": "the water tracker"})),
        ("rename_vague", "rename_app", json!({"app": "water", "title": "X"})),
        ("habit_word", "delete_app", json!({"app": "my habit journal"})),
    ] {
        out.insert(name, door.handle(tool, &args(a), 7, chat.send(), chat.say(), None).await);
    }
    let mut kept: Vec<String> = store.list().into_iter().map(|a| a.slug).collect();
    kept.sort();
    let versions = store.info("habit-tracker").unwrap().versions;
    let sure = door.handle("delete_app", &args(json!({"app": "the water plants"})), 7, chat.send(), chat.say(), None).await;
    let mut after: Vec<String> = store.list().into_iter().map(|a| a.slug).collect();
    after.sort();
    assert_eq!(kept, ["habit-tracker", "water-log", "water-plants"]); // nothing happened
    assert_eq!(versions, 1);
    assert_eq!(out["both_water"]["ok"], json!(false));
    let mut candidates: Vec<String> =
        out["both_water"]["candidates"].as_array().unwrap().iter().map(|c| c.as_str().unwrap().to_string()).collect();
    candidates.sort();
    assert_eq!(candidates, ["Water Log", "Water Plants"]);
    for key in ["no_such", "undo_no_such", "rename_vague", "habit_word"] {
        assert_eq!(out[key]["ok"], json!(false), "{key}");
        assert!(out[key]["reason"].as_str().unwrap().contains("nothing was done"), "{key}");
    }
    assert_eq!(sure["ok"], json!(true));
    assert_eq!(after, ["habit-tracker", "water-log"]);
}

#[test]
fn test_same_titled_apps_are_told_apart_by_slug_when_the_owner_is_asked() {
    let t = tmp();
    let store = AppStore::new(t.path());
    store.save(&doc_titled("a", "Notes"), "a", "", None, None).unwrap();
    store.save(&doc_titled("b", "Notes"), "b", "", None, None).unwrap();
    let (app, candidates) = store.resolve("notes"); // the slug "notes" is exact: that one
    assert!(app.is_some_and(|a| a.slug == "notes") && candidates.is_empty());
    let (app, _) = store.resolve("Notes ");
    assert!(app.is_some_and(|a| a.slug == "notes"));
    store.rename("notes", "Journal").unwrap();
    store.rename("notes-2", "Journal").unwrap();
    let (app, candidates) = store.resolve("journal");
    let mut slugs: Vec<String> = candidates.into_iter().map(|c| c.slug).collect();
    slugs.sort();
    assert!(app.is_none());
    assert_eq!(slugs, ["notes", "notes-2"]);
}

#[test]
fn test_a_deleted_app_comes_back_with_its_data() {
    let t = tmp();
    let store = AppStore::new(t.path());
    let slug = store.save(DOC, "x", "", None, None).unwrap().slug;
    store.save_data(&slug, &json!({"v": 1, "habits": ["read"]})).unwrap();
    store.delete(&slug).unwrap();
    let titles: Vec<String> = store.trashed().into_iter().map(|a| a.title).collect();
    assert_eq!(titles, ["Habit Tracker"]);
    assert!(store.info(&slug).is_none());
    let back = store.restore(&slug).unwrap();
    assert_eq!(back.slug, slug);
    assert_eq!(store.load_data(&slug).unwrap(), json!({"v": 1, "habits": ["read"]}));
    assert!(store.trashed().is_empty());
    store.delete(&slug).unwrap();
    store.save(&doc("new"), "a new one with the same name", "", None, None).unwrap(); // the address is taken again
    assert!(matches!(store.restore(&slug), Err(StoreError::Exists(_))));
    assert!(missing(store.restore("never-was")));
    assert!(missing(store.restore("../x")));
}

#[tokio::test]
#[ignore = "needs ChatMaker from the apps_maker unit (not filled in this unit; un-ignore once it merges)"]
async fn test_restore_from_the_chat() {
    let t = tmp();
    let chat = Chat::default();
    let (door, store, _) = door_for(t.path(), &Script::texts(&[fenced(DOC)]), None);
    store.save(DOC, "x", "", None, None).unwrap();
    door.handle("delete_app", &args(json!({"app": "habit tracker"})), 7, chat.send(), chat.say(), None).await;
    let missing_app = door.handle("restore_app", &args(json!({"app": "zebra"})), 7, chat.send(), chat.say(), None).await;
    let back = door.handle("restore_app", &args(json!({"app": "the habit tracker"})), 7, chat.send(), chat.say(), None).await;
    assert_eq!(missing_app["ok"], json!(false));
    assert!(missing_app["reason"].as_str().unwrap().contains("Habit Tracker"));
    assert!(back["ok"] == json!(true) && back["restored"] == json!("Habit Tracker"));
    assert!(store.info("habit-tracker").is_some());
    let last = chat.sent.lock().last().cloned().unwrap();
    assert_eq!(last, (7, "✅ Habit Tracker is back, with its data.".to_string(), vec![open_button()]));
}

#[test]
fn test_undo_warns_when_the_data_has_moved_on_since_that_version() {
    let t = tmp();
    let store = AppStore::new(t.path());
    let slug = store.save(&doc("v1"), "first", "", None, None).unwrap().slug;
    store.save_data(&slug, &json!({"v": 1, "habits": []})).unwrap();
    store.save(&doc("v2"), "add categories", &slug, None, None).unwrap(); // the page v1 was written for data v1
    assert!(!store.info(&slug).unwrap().undo_warning); // nothing migrated yet
    store.save_data(&slug, &json!({"v": 2, "habits": [], "categories": []})).unwrap();
    assert!(store.info(&slug).unwrap().undo_warning);
    assert_eq!(store.info(&slug).unwrap().as_dict()["undo_warning"], json!(true));
    store.revert(&slug).unwrap();
    assert!(!store.info(&slug).unwrap().undo_warning); // no earlier version left to warn about
}

#[tokio::test]
#[ignore = "needs ChatMaker from the apps_maker unit (not filled in this unit; un-ignore once it merges)"]
async fn test_undo_from_the_chat_says_when_newer_entries_may_not_open() {
    let t = tmp();
    let chat = Chat::default();
    let (door, store, _) = door_for(t.path(), &Script::texts(&[fenced(DOC)]), None);
    store.save(&doc("v1"), "first", "", None, None).unwrap();
    store.save_data("habit-tracker", &json!({"v": 1})).unwrap();
    store.save(&doc("v2"), "change", "habit-tracker", None, None).unwrap();
    store.save_data("habit-tracker", &json!({"v": 2})).unwrap();
    let out = door.handle("undo_app", &args(json!({"app": "habit tracker"})), 7, chat.send(), chat.say(), None).await;
    assert_eq!(out["ok"], json!(true));
    assert!(out["note"].as_str().unwrap().contains("newer shape"));
    assert!(chat.sent.lock().last().unwrap().1.contains("may not open"));
}

#[test]
fn test_a_library_must_be_an_npm_package_at_an_exact_version() {
    for (url, ok) in [
        ("https://cdn.jsdelivr.net/npm/chart.js@4.4.1/dist/chart.umd.min.js", true),
        ("https://cdn.jsdelivr.net/npm/@kurkle/color@0.3.2/dist/color.min.js", true),
        ("https://cdn.jsdelivr.net/npm/chart.js@4.4.1", true),
        ("https://cdn.jsdelivr.net/npm/chart.js/dist/chart.umd.min.js", false), // no version: changes
        ("https://cdn.jsdelivr.net/npm/chart.js@latest/dist/chart.umd.min.js", false),
        ("https://cdn.jsdelivr.net/npm/chart.js@4/dist/chart.umd.min.js", false), // a range
        ("https://cdn.jsdelivr.net/gh/someone/lib@main/x.js", false),             // anyone's branch
        ("https://cdn.jsdelivr.net/combine/npm/a@1.0.0,npm/b", false),
    ] {
        let page = DOC.replace("hi", &format!("<script src=\"{url}\"></script>"));
        assert_eq!(load_issues(&page).is_empty(), ok, "{url}");
        assert_eq!(static_issues(&page).is_empty(), ok, "{url}");
    }
}

#[tokio::test]
#[ignore = "needs AppMaker from the apps_maker unit (not filled in this unit; un-ignore once it merges)"]
async fn test_a_version_that_loads_outside_code_is_never_saved() {
    let t = tmp();
    let unpinned = DOC.replace("hi", "<script src=\"https://cdn.jsdelivr.net/npm/chart.js/dist/chart.umd.min.js\"></script>");
    let (maker, _, store) = maker_for(t.path(), Script::texts(&[fenced(&unpinned)]).generate(), None);
    let made = maker.make("a chart", None).await;
    assert!(!made.ok && made.reason.contains("outside") && store.list().is_empty());
    let pinned = unpinned.replace("chart.js/dist", "chart.js@4.4.1/dist");
    let (maker, _, store) = maker_for(&t.path().join("b"), Script::texts(&[fenced(&unpinned), fenced(&pinned)]).generate(), None);
    let made = maker.make("a chart", None).await;
    assert!(made.ok && made.rounds == 1);
    assert!(store.html(&made.app.unwrap().slug).unwrap().contains("chart.js@4.4.1"));
}

#[tokio::test]
#[ignore = "needs AppMaker from the apps_maker unit (not filled in this unit; un-ignore once it merges)"]
async fn test_a_version_the_check_could_not_run_on_never_beats_a_tested_one() {
    let t = tmp();
    let flaky = FakeCheck { skip: Some("v2".into()), ..FakeCheck::new(&[("v1", &["a", "b", "c"])]) };
    let gen = Script::texts(&[fenced(&doc("v1")), fenced(&doc("v2"))]);
    let (maker, _, store) = maker_for(t.path(), gen.generate(), Some(flaky.check()));
    let made = maker.make("habit tracker", None).await;
    assert!(made.ok && store.html(&made.app.as_ref().unwrap().slug).unwrap().contains("v1"));
    assert_eq!(made.issues, ["a", "b", "c"]);
    assert!(made.check.as_ref().is_some_and(|c| c.skipped.is_empty()));
}

#[tokio::test]
#[ignore = "needs ChatMaker from the apps_maker unit and miniapp's error_line (not filled in this unit; un-ignore once they merge)"]
async fn test_a_chat_change_that_fails_says_why_and_that_the_app_is_unchanged() {
    let t = tmp();
    let chat = Chat::default();
    let (door, store, spawned) = door_for(t.path(), &Script::new(vec![Answer::Unreachable]), None);
    store.save(DOC, "x", "", None, None).unwrap();
    door.handle("change_app", &args(json!({"app": "habit tracker", "change": "add a chart"})), 7, chat.send(), chat.say(), None)
        .await;
    door.handle("make_app", &args(json!({"request": "a reading list"})), 7, chat.send(), chat.say(), None).await;
    spawned.gather().await;
    let first = chat.sent.lock()[0].clone();
    assert_eq!(
        (first.1.as_str(), first.2),
        (
            "I couldn't change Habit Tracker; it is unchanged: Claude could not be reached. Check the Mac's internet \
             connection.",
            vec![change_button()]
        )
    );
    // no app, no chat
    assert!(chat.said.lock()[0].starts_with("I couldn't build that app: Claude could not be reached."));
}

#[tokio::test]
#[ignore = "needs ChatMaker from the apps_maker unit, and a seam for CHAT_PROGRESS_SECS (Python patches it to 0)"]
async fn test_a_chat_build_says_when_it_is_tested_and_fixed_and_what_may_still_be_wrong() {
    let t = tmp();
    let chat = Chat::default();
    let check = FakeCheck::new(&[
        ("v1", &["Tapping Add threw."]),
        ("v2", &["Tapping Add threw."]),
        ("v3", &["Tapping Add threw."]),
    ]);
    let gen = Script::texts(&[fenced(&doc("v1")), fenced(&doc("v2")), fenced(&doc("v3"))]);
    let (door, _, spawned) = door_for(t.path(), &gen, Some(check.check()));
    door.handle("make_app", &args(json!({"request": "habit tracker"})), 7, chat.send(), chat.say(), Some(chat.send_photo()))
        .await;
    spawned.gather().await;
    tokio::task::yield_now().await;
    assert_eq!(
        *chat.said.lock(),
        [
            "Written. Testing it on a phone…",
            "The phone test found 1 problem; fixing it…",
            "The phone test found 1 problem; fixing it…"
        ]
    );
    let caption = chat.photos.lock()[0].2.clone();
    assert!(caption.contains("It may still have a problem: Tapping Add threw. Tell me what to fix."));
    assert!(caption.ends_with("(Picture: a test run with sample entries.)"));
}

// ---- the way into an app's change chat from inside the app (eve.js) -----------------------------------------------

#[tokio::test]
async fn test_inside_an_app_a_pencil_and_telegrams_settings_item_lead_to_its_change_chat() {
    if find_chromium().is_none() {
        return;
    }
    let stub = r#"window.Telegram = { WebApp: { isVersionAtLeast: (v) => parseFloat(v) <= 7.0,
      BackButton: { show() {}, hide() {}, onClick() {}, offClick() {} },
      SettingsButton: (() => { const cbs = []; const b = { isVisible: false, show() { b.isVisible = true; return b; },
        onClick(f) { cbs.push(f); return b; }, press() { cbs.forEach((f) => f()); } }; return b; })() } };"#;
    // An app whose own CSS would hide or restyle a plain injected button, and that re-renders its whole body.
    let hostile_css = r#"<style>* { visibility: hidden !important; position: static !important; }
      button { display: none !important; } html, body { overflow: hidden; }</style>"#;
    let body = r#"<h1>A very long app title that runs</h1><script>document.body.innerHTML = '<p>re-rendered</p>';</script>"#;
    let page_html =
        format!("<html><head><script>{stub}</script><script>{EVE_JS}</script>{hostile_css}</head><body>{body}</body></html>");
    let b = Browser::launch(&[]).await.expect("chromium");
    let ctx = b.new_context(phone(390, 760, true)).await.expect("context");
    let page = ctx.new_page().await.expect("page");
    serve_app_test(&page, page_html).await;
    page.goto("https://app.test/apps/habit-tracker/?t=abc", 30_000).await.expect("goto");
    let seen = page
        .evaluate(
            r#"() => { const host = document.querySelector('eve-change');
              const r = host.getBoundingClientRect(), s = getComputedStyle(host);
              const top = document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2);
              return { w: r.width, h: r.height, right: innerWidth - r.right, top: r.top, visible: s.visibility,
                       topmost: top === host, count: document.querySelectorAll('eve-change').length,
                       settings: Telegram.WebApp.SettingsButton.isVisible,
                       wide: document.documentElement.scrollWidth > innerWidth }; }"#,
            None,
        )
        .await
        .expect("seen");
    // page.touchscreen.tap(390 - 6 - 22, 6 + 22)
    let tap = async {
        let at = json!([{"x": 390 - 6 - 22, "y": 6 + 22}]);
        let _ = page.call("Input.dispatchTouchEvent", json!({"type": "touchStart", "touchPoints": at})).await;
        let _ = page.call("Input.dispatchTouchEvent", json!({"type": "touchEnd", "touchPoints": []})).await;
    };
    let tapped = navigation(&page, tap).await;
    page.goto("https://app.test/apps/habit-tracker/?t=abc", 30_000).await.expect("goto");
    let settings = navigation(&page, evaluate_leaving(&page, "Telegram.WebApp.SettingsButton.press()")).await;
    b.close().await;
    assert_eq!(
        seen,
        json!({"w": 44, "h": 44, "right": 6, "top": 6, "visible": "visible", "topmost": true, "count": 1,
               "settings": true, "wide": false})
    );
    assert_eq!(tapped, "https://app.test/?chat=habit-tracker");
    assert_eq!(settings, "https://app.test/?chat=habit-tracker");
}

#[tokio::test]
async fn test_without_telegrams_settings_item_the_pencil_still_leads_there() {
    if find_chromium().is_none() {
        return;
    }
    let old = "window.Telegram = { WebApp: { isVersionAtLeast: (v) => parseFloat(v) <= 6.9 } };";
    let page_html = format!("<html><head><script>{old}</script><script>{EVE_JS}</script></head><body>app</body></html>");
    let b = Browser::launch(&[]).await.expect("chromium");
    let ctx = b.new_context(phone(1280, 720, false)).await.expect("context");
    let page = ctx.new_page().await.expect("page");
    let mut events = page.take_events().expect("events");
    page.call("Runtime.enable", json!({})).await.expect("runtime");
    page.route_all().await.expect("route");
    let errors: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let (p, errs, html) = (page.clone(), errors.clone(), page_html.clone());
    tokio::spawn(async move {
        while let Some(ev) = events.recv().await {
            match ev.method.as_str() {
                "Fetch.requestPaused" => {
                    let id = ev.params.get("requestId").and_then(Value::as_str).unwrap_or("").to_string();
                    let _ = p.fulfill(&id, 200, &[("content-type", "text/html")], &html).await;
                }
                // page.on("pageerror")
                "Runtime.exceptionThrown" => errs.lock().push(ev.params.to_string()),
                _ => {}
            }
        }
    });
    page.goto("https://app.test/apps/x/?t=abc", 30_000).await.expect("goto");
    let present = page.evaluate("document.querySelector('eve-change') !== null", None).await.expect("label");
    b.close().await;
    assert_eq!(present, json!(true));
    assert!(errors.lock().is_empty(), "{:?}", errors.lock());
}

#[tokio::test]
#[ignore = "needs ChatMaker from the apps_maker unit (not filled in this unit; un-ignore once it merges)"]
async fn test_a_change_from_the_chat_door_brings_a_change_button_to_its_chat() {
    let t = tmp();
    let chat = Chat::default();
    let (door, _, spawned) = door_for(t.path(), &Script::texts(&[fenced(DOC), fenced(&doc("v2"))]), None);
    door.handle("make_app", &args(json!({"request": "habit tracker"})), 7, chat.send(), chat.say(), Some(chat.send_photo()))
        .await;
    spawned.gather().await;
    door.handle(
        "change_app",
        &args(json!({"app": "habit tracker", "change": "add categories"})),
        7,
        chat.send(),
        chat.say(),
        Some(chat.send_photo()),
    )
    .await;
    spawned.gather().await;
    let photos = chat.photos.lock().clone();
    assert_eq!(photos.len(), 2);
    // Open stays Open <app>; Change is second
    assert_eq!(photos[0].3, vec![open_button()]);
    assert_eq!(photos[1].3, vec![open_button(), change_button()]);
}

// ---- the store's own pieces (apps_maker.py's helpers, as the tests above reach them) ------------------------------

#[test]
fn test_meta_written_by_python_reads_back_and_a_saved_meta_matches_its_bytes() {
    // meta.json and data.json are shared with the Python daemon: Python's json.dumps spacing, non-ASCII kept in
    // meta.json and escaped in data.json, journeys.json indented by one.
    let t = tmp();
    let store = AppStore::new(t.path());
    let journeys = vec![eve_jev_verify::Journey {
        name: "Add one".into(),
        steps: vec![eve_jev_verify::Step::tap("Add")],
        expect: vec!["1 habit".into()],
    }];
    let slug = store.save(DOC, "a habit tracker", "", Some(&journeys), None).unwrap().slug;
    let meta = std::fs::read_to_string(t.path().join(&slug).join("meta.json")).unwrap();
    assert!(meta.contains("\"icon\": \"✅\"") && meta.contains("\"seq\": 1, "), "{meta}");
    let raw = std::fs::read_to_string(t.path().join(&slug).join("journeys.json")).unwrap();
    assert!(raw.starts_with("[\n {\n  \"name\": \"Add one\",\n  \"steps\": [\n   {"), "{raw}");
    assert_eq!(store.journeys(&slug).unwrap(), journeys);
    assert_eq!(store.save_data(&slug, &json!({"name": "é"})).unwrap(), 18);
    assert_eq!(std::fs::read_to_string(t.path().join(&slug).join("data.json")).unwrap(), "{\"name\": \"\\u00e9\"}");
    // an undo to a page built before journeys drops the journeys file
    store.save(&doc("v2"), "change", &slug, None, None).unwrap();
    assert!(store.journeys(&slug).is_none());
    store.revert(&slug).unwrap();
    assert_eq!(store.journeys(&slug).unwrap(), journeys);
}

#[test]
fn test_the_thread_keeps_requests_their_results_and_the_builds_that_saved_nothing() {
    let t = tmp();
    let store = AppStore::new(t.path());
    let rec = |ok: bool| {
        eve_brain::apps_maker::build_record(ok, None, &["x".to_string()], 0.123456, 61.5, 1, if ok { "" } else { "no" })
    };
    let slug = store.save(DOC, "first", "", None, Some(&rec(true))).unwrap().slug;
    store.record_build(&slug, "a change that failed", &rec(false)).unwrap();
    let thread = store.thread(&slug).unwrap();
    let kinds: Vec<(&str, &str)> =
        thread.iter().map(|i| (i["text"].as_str().unwrap(), i["kind"].as_str().unwrap())).collect();
    assert_eq!(kinds, [("first", "request"), ("a change that failed", "request")]);
    assert_eq!(thread[0]["result"]["version"], json!(1));
    assert_eq!(thread[0]["result"]["usd"], json!(0.1235));
    assert_eq!(thread[0]["result"]["secs"], json!(62));
    assert_eq!(thread[0]["result"]["check"], json!("not tested"));
    assert_eq!(thread[1]["result"]["ok"], json!(false));
    assert_eq!(thread[1]["result"]["version"], Value::Null);
    assert!(missing(store.record_build("no-such-app", "x", &args(json!({"ok": false})))));
    assert!(missing(store.thread("no-such-app")));
}

#[test]
fn test_the_store_is_eve_links_app_list() {
    use eve_link::seams::AppStoreView;
    let t = tmp();
    let store = AppStore::new(t.path());
    let a = store.save(DOC, "x", "", None, None).unwrap();
    let view: &dyn AppStoreView = &store;
    let listed = view.list();
    assert_eq!(listed.len(), 1);
    assert!(listed[0].slug == "habit-tracker" && listed[0].title == "Habit Tracker" && listed[0].updated > 1.0e9);
    assert_eq!(view.info(&a.slug).unwrap(), listed[0]);
    assert!(view.info("nope").is_none());
}
