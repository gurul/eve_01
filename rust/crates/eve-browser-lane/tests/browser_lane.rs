//! Port of test_browser_lane.py (and the lane's lines in test_app_reflex.py, test_vault_fill.py): a page as a
//! Snapshot, the browser as the hands, the plan executor unchanged on top.
//!
//! The live tests launch the real headless Chromium (Playwright's, from its cache) over the DevTools protocol
//! against local pages (no network). They are `#[ignore]`d; run them with `cargo test -- --ignored`.

mod common;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use common::*;
use eve_browser_lane as bl;
use eve_browser_lane::driver::{PageHandle, SelectBy};
use indexmap::IndexMap;
use parking_lot::Mutex;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

// ---- pure ---------------------------------------------------------------------------------------------------

#[test]
fn test_web_goals_are_decided_by_code() {
    for web in [
        "open google news and show me the top headline",
        "go to https://example.com",
        "search the web for ramen",
        "look up the weather",
        "open amazon",
        "on github open my pull requests",
        "in the browser open bbc",
    ] {
        assert!(bl::is_web_goal(web, ""), "{web}");
    }
    for mac in ["open the calculator", "pause the music", "put the calendar on the year view", "open mail"] {
        assert!(!bl::is_web_goal(mac, ""), "{mac}");
    }
    assert!(bl::is_web_goal("anything", "search") && bl::is_web_goal("anything", "open_url"));
}

#[test]
fn test_open_it_after_a_shared_link_is_a_web_goal() {
    // test_app_reflex.py: the goal app_reflex builds carries the shared link, and the lane takes it.
    assert!(bl::is_web_goal("Use Google search to open it up https://maps.app.goo.gl/AbCdEf123", ""));
}

#[test]
fn test_a_page_becomes_a_snapshot_the_gate_can_read() {
    let raw = json!({"title": "Eve Shop", "url": "file:///shop.html", "dialog": "", "elements": [
        {"id": "0", "role": "link", "name": "Top stories", "value": "", "x": 10, "y": 40, "w": 80, "h": 20, "enabled": true,
         "secure": false, "editable": false, "dialog": false, "focused": false},
        {"id": "1", "role": "searchbox", "name": "Search products", "value": "", "x": 10, "y": 80, "w": 200, "h": 30,
         "enabled": true, "secure": false, "editable": true, "dialog": false, "focused": true},
        {"id": "2", "role": "password", "name": "Password", "value": "", "x": 10, "y": 120, "w": 200, "h": 30,
         "enabled": true, "secure": true, "editable": false, "dialog": false, "focused": false},
        {"id": "3", "role": "tab", "name": "Week", "value": "selected", "x": 10, "y": 160, "w": 60, "h": 30,
         "enabled": true, "secure": false, "editable": false, "dialog": false, "focused": false}]});
    let snap = bl::snapshot_from_page(&raw, 3, 0.01).unwrap();
    assert!(snap.app == "browser" && snap.title == "Eve Shop" && snap.seq == 3 && snap.pid == 1);
    let roles: Vec<&str> = snap.elements.iter().map(|c| c.role.as_str()).collect();
    assert_eq!(roles, ["link", "search field", "secure text field", "tab"]);
    let focused = snap.focused.as_ref().unwrap();
    assert!(focused.label == "Search products" && focused.editable);
    assert!(snap.get("2").unwrap().secure && snap.get("3").unwrap().value == "selected");
    assert!(snap.context_lines.contains(&"url: file:///shop.html".to_string()));
    let lines = bl::outline_lines(&snap, bl::OUTLINE_LIMIT);
    assert_eq!(lines, ["link: Top stories", "search field: Search products", "tab: Week, selected"]); // no secure field
}

#[test]
fn snapshot_rows_fail_as_python_fails() {
    // `int(e.get("x", 0))` of a word is a ValueError; a row that is not a mapping has no `.get`.
    let bad = bl::snapshot_from_page(&json!({"elements": [{"id": "0", "x": "left"}]}), 1, 0.0).unwrap_err();
    assert_eq!(eve_desktop_lanes::error_kind(&bad), "ValueError");
    let bad = bl::snapshot_from_page(&json!({"elements": ["x"]}), 1, 0.0).unwrap_err();
    assert_eq!(eve_desktop_lanes::error_kind(&bad), "AttributeError");
    // an id that is a number is its str(); rows sort by (y // 8, x)
    let snap = bl::snapshot_from_page(
        &json!({"elements": [{"id": 7, "y": 17, "x": 5}, {"id": 8, "y": 16, "x": 1}, {"id": 9, "y": 0, "x": 50}]}),
        1,
        0.0,
    )
    .unwrap();
    let ids: Vec<&str> = snap.elements.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, ["9", "8", "7"]);
    assert_eq!(snap.elements[0].role, "control");
}

#[test]
fn test_a_consent_notice_is_dismissed_only_by_refusing_or_closing() {
    let names = |xs: &[&str]| -> Vec<Value> {
        xs.iter().enumerate().map(|(i, x)| json!({"id": i.to_string(), "name": x})).collect()
    };
    let pick = |xs: &[&str]| bl::consent_choice(&names(xs)).map(|b| b["name"].as_str().unwrap().to_string());
    assert!(pick(&["Accept all cookies", "Reject non-essential cookies"]).unwrap().starts_with("Reject"));
    assert_eq!(pick(&["Agree", "Necessary only"]).unwrap(), "Necessary only");
    assert_eq!(pick(&["OK", "Close"]).unwrap(), "Close");
    for agreeing in [&["Accept all cookies"][..], &["I agree", "OK"], &["Accept necessary cookies"], &[]] {
        assert_eq!(pick(agreeing), None, "{agreeing:?}");
    }
    assert!(bl::CONSENT_TEXT.is_match("We use cookies to improve your experience."));
    assert!(!bl::CONSENT_TEXT.is_match("Delete this event?"));
}

const ROUTE_APPS: [&str; 9] = ["Spotify", "Photo Booth", "Slack", "News", "Weather", "Maps", "Google Chrome", "Safari", "Notes"];

#[test]
fn test_only_a_browser_only_goal_goes_to_the_chrome_lane() {
    let apps: Vec<String> = ROUTE_APPS.iter().map(|s| s.to_string()).collect();
    let only = |g: &str| bl::is_browser_only_goal(g, &apps);
    for goal in [
        "open up Google",
        "search the latest news on Google",
        "check my gmail",
        "look up the weather",
        "open youtube and play lofi",
        "open github.com",
        "open spotify in chrome",
        "google how to reset spotify",
        "look up how to use Photo Booth",
    ] {
        assert!(only(goal), "{goal}");
    }
    for goal in [
        "Open Spotify on my computer and on my browser",
        "open spotify and chrome",
        "Send the picture I just took on Photo Booth to me",
        "play my liked songs on Spotify",
        "open the desktop app for youtube",
        "open Notes",
    ] {
        assert!(!only(goal), "{goal}");
    }
}

#[test]
fn the_page_scripts_are_pythons_byte_for_byte() {
    // Lengths (characters) and sha256 prefixes measured from browser_lane.py's own constants.
    for (name, js, len, sha) in [
        ("_IDENTITY_JS", bl::IDENTITY_JS, 1971, "1d5684f0b0d5bfca"),
        ("_COLLECT_JS", bl::COLLECT_JS, 5290, "65da978494153ab0"),
        ("_RECHECK_JS", bl::RECHECK_JS, 2329, "80a672486b1b511f"),
        ("_WEBMCP_JS", bl::WEBMCP_JS, 474, "55e0858be793e83e"),
        ("_CONSENT_JS", bl::CONSENT_JS, 1161, "4abbbe045bd4446e"),
        ("_FILL_FIELDS_JS", bl::FILL_FIELDS_JS, 4203, "e13baa8bae2bd35e"),
    ] {
        assert_eq!(js.chars().count(), len, "{name}");
        let hex: String = Sha256::digest(js.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
        assert!(hex.starts_with(sha), "{name}: {hex}");
    }
}

#[test]
fn make_step_asker_follows_eve_jev_step() {
    let env = |pairs: &[(&str, &str)]| -> std::collections::HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    };
    assert!(bl::make_step_asker(Some(&env(&[]))).is_none());
    assert!(bl::make_step_asker(Some(&env(&[("EVE_JEV_STEP", "off")]))).is_none());
}

#[test]
fn press_names_only_the_keys_a_plan_may_press() {
    let page = FakePage::new("https://shop.example/", "Eve Shop", "Welcome", vec![]);
    let p = bl::Page::new(page.boxed());
    assert_eq!(p.press("Escape").unwrap(), "pressed Escape");
    let err = p.press("f13").unwrap_err();
    assert_eq!(eve_desktop_lanes::error_kind(&err), "ValueError");
    assert_eq!(page.keys(), ["Escape"]);
}

// ---- the vault fill refuses before touching the page (test_vault_fill.py) --------------------------------------

/// A page that records every call made on it, answering each with an empty value.
#[derive(Clone, Default)]
struct RecordingPage(Arc<Mutex<Vec<String>>>);

impl RecordingPage {
    fn log(&self, name: &str) {
        self.0.lock().push(name.into());
    }
}

impl PageHandle for RecordingPage {
    fn evaluate(&self, _s: &str, _a: Option<&Value>) -> anyhow::Result<Value> {
        self.log("evaluate");
        Ok(json!([]))
    }
    fn title(&self) -> anyhow::Result<String> {
        self.log("title");
        Ok(String::new())
    }
    fn url(&self) -> String {
        "https://chase.com.evil.example/login".into()
    }
    fn inner_text(&self, _s: &str, _t: u64) -> anyhow::Result<String> {
        self.log("inner_text");
        Ok(String::new())
    }
    fn count(&self, _s: &str) -> anyhow::Result<usize> {
        self.log("count");
        Ok(0)
    }
    fn scroll_into_view_if_needed(&self, _s: &str, _t: u64) -> anyhow::Result<()> {
        self.log("scroll_into_view_if_needed");
        Ok(())
    }
    fn click(&self, _s: &str, _t: u64) -> anyhow::Result<()> {
        self.log("click");
        Ok(())
    }
    fn fill(&self, _s: &str, _v: &str, _t: u64) -> anyhow::Result<()> {
        self.log("fill");
        Ok(())
    }
    fn select_option(&self, _s: &str, _c: SelectBy<'_>, _t: u64) -> anyhow::Result<()> {
        self.log("select_option");
        Ok(())
    }
    fn evaluate_on(&self, _s: &str, _script: &str) -> anyhow::Result<Value> {
        self.log("evaluate_on");
        Ok(json!([]))
    }
    fn keyboard_type(&self, _t: &str, _d: u64) -> anyhow::Result<()> {
        self.log("keyboard_type");
        Ok(())
    }
    fn keyboard_press(&self, _k: &str) -> anyhow::Result<()> {
        self.log("keyboard_press");
        Ok(())
    }
    fn wait_for_timeout(&self, _ms: u64) {
        self.log("wait_for_timeout");
    }
    fn wait_for_load_state(&self, _s: &str, _t: u64) -> anyhow::Result<()> {
        self.log("wait_for_load_state");
        Ok(())
    }
    fn goto(&self, _u: &str, _w: &str, _t: u64) -> anyhow::Result<()> {
        self.log("goto");
        Ok(())
    }
    fn bring_to_front(&self) -> anyhow::Result<()> {
        self.log("bring_to_front");
        Ok(())
    }
    fn close(&self) -> anyhow::Result<()> {
        self.log("close");
        Ok(())
    }
    fn screenshot_jpeg(&self, _q: u8, _m: Option<(&str, &str)>) -> anyhow::Result<Vec<u8>> {
        self.log("screenshot");
        Ok(vec![])
    }
}

fn mem_vault(dir: &Path) -> Arc<eve_vault::Vault> {
    Arc::new(eve_vault::Vault::with_store(dir.join("vault.json"), Box::new(eve_vault::MemKeychain::new())))
}

fn values(pairs: &[(&str, &str)]) -> eve_vault::Values {
    pairs.iter().map(|(k, v)| (k.to_string(), json!(v))).collect()
}

#[test]
fn test_the_page_refuses_before_touching_the_page_off_site_or_without_a_vault() {
    let tmp = tempfile::tempdir().unwrap();
    let v = mem_vault(tmp.path());
    let item = v.add("login", "Chase", &values(&[("username", "me"), ("password", "pw")]), "chase.com").unwrap();
    let page = RecordingPage::default();
    let vault: Option<Arc<dyn bl::LaneVault>> = Some(v.clone());
    let said = bl::Page::new(Box::new(page.clone())).with_vault(vault.clone()).fill_from_vault(&item.handle).unwrap();
    assert!(said.starts_with("refused:") && said.contains("saved for chase.com"), "{said}");
    assert!(page.0.lock().is_empty());
    let none = bl::Page::new(Box::new(page.clone())).fill_from_vault(&item.handle).unwrap();
    assert_eq!(none, "refused: there is no vault");
    let missing = bl::Page::new(Box::new(page.clone())).with_vault(vault).fill_from_vault("v_zzzzzzzz").unwrap();
    assert!(missing.starts_with("refused: no vault item"), "{missing}");
}

// ---- live: a real Chromium through the real executor ---------------------------------------------------------

const PAGE: &str = r##"<!doctype html><html><head><title>Eve Shop</title></head><body>
<h1>Welcome</h1>
<nav><a href="#news">Top stories</a> <a href="#account">Your account</a></nav>
<form onsubmit="event.preventDefault(); document.getElementById('out').textContent='Results for '+document.getElementById('q').value; document.title='Results - Eve Shop';">
  <label for="q">Search products</label><input id="q" type="search" placeholder="Search">
  <button type="submit">Search</button>
</form>
<label><input type="checkbox" id="c1"> Email me deals</label>
<div role="tablist"><button role="tab" aria-selected="true">Week</button><button role="tab" aria-selected="false">Month</button></div>
<button onclick="document.getElementById('out').textContent='Order placed'">Place Order</button>
<input type="password" aria-label="Password">
<p id="out"></p>
<a href="#" style="display:none">Hidden link</a>
</body></html>"##;

/// A static file server on loopback (Python's SimpleHTTPRequestHandler, GET only): the query is ignored.
fn serve(dir: &Path) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let root: PathBuf = dir.into();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let root = root.clone();
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
                let head = String::from_utf8_lossy(&buf).into_owned();
                let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                let path = path.split(['?', '#']).next().unwrap_or("/").trim_start_matches('/').to_string();
                let reply = match std::fs::read(root.join(&path)) {
                    Ok(body) => {
                        let mut r = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        )
                        .into_bytes();
                        r.extend(body);
                        r
                    }
                    Err(_) => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
                };
                let _ = stream.write_all(&reply);
            });
        }
    });
    format!("http://127.0.0.1:{port}")
}

fn live_lane(tmp: &Path, vault: Option<Arc<dyn bl::LaneVault>>, plain_http: &[(&str, &str)]) -> bl::BrowserLane {
    let cfg = bl::BrowserLaneConfig { profile: tmp.join("profile"), headless: true, ..Default::default() };
    let mut core = bl::LaneCore::new(cfg, bl::LaneOptions { vault: vault.or_else(no_vault), ..Default::default() }).unwrap();
    for (from, to) in plain_http {
        core.plain_http.insert(from.to_string(), to.to_string());
    }
    bl::BrowserLane::from_core(core).unwrap()
}

fn file_uri(p: &Path) -> String {
    format!("file://{}", p.display())
}

#[test]
#[ignore = "live: launches Playwright's headless Chromium"]
fn test_the_page_senses_and_effectors_keep_the_lane_contract() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("shop.html");
    std::fs::write(&f, PAGE).unwrap();
    let lane = live_lane(tmp.path(), None, &[]);
    run(async {
        assert!(lane.open_url(&file_uri(&f)).await.unwrap().starts_with("opened "));
        let outline = lane.outline().await.unwrap();
        assert!(outline.app == "browser" && outline.title == "Eve Shop");
        let has = |l: &str| outline.lines.iter().any(|x| x == l);
        assert!(has("button: Search") && has("link: Top stories"), "{:?}", outline.lines);
        assert!(has("checkbox: Email me deals, unchecked") && has("tab: Week, selected"), "{:?}", outline.lines);
        assert!(!outline.lines.iter().any(|l| l.contains("Password") || l.contains("Hidden")));
        let out = lane
            .run(|c| -> anyhow::Result<Value> {
                let p = c.ensure()?;
                let snap = p.snapshot()?;
                let find = |label: &str| snap.elements.iter().find(|c| c.label == label).cloned().unwrap();
                let (bx, field, order) = (find("Email me deals"), find("Search products"), find("Place Order"));
                let pw = snap.elements.iter().find(|c| c.secure).cloned().unwrap();
                let click = p.click_candidate(&bx);
                let after = p.snapshot()?;
                let box_after = after.elements.iter().find(|c| c.label == "Email me deals").unwrap().value.clone();
                let changed = p.screen_changed();
                let sensitive = p.click_candidate(&order);
                p.set_approve(Some("place order"));
                let approved = p.click_candidate(&order);
                let order_text = p.text_visible("Order placed");
                let secure = p.focus_and_type(&pw, "hunter2");
                let typed = p.focus_and_type(&field, "ramen");
                let pressed = p.press("return")?;
                p.settle(0.5);
                let results = p.text_visible("Results for ramen");
                let title = p.snapshot()?.title;
                let focused = p.focused()?.map(|c| c.label);
                assert!(p.press("f13").is_err());
                Ok(json!({"click": click, "box_after": box_after, "changed": changed, "sensitive": sensitive,
                          "approved": approved, "order_text": order_text, "secure": secure, "typed": typed,
                          "pressed": pressed, "results": results, "title": title, "focused": focused}))
            })
            .await
            .unwrap()
            .unwrap();
        let s = |k: &str| out[k].as_str().unwrap_or("").to_string();
        assert!(s("click").starts_with("clicked checkbox 'Email me deals'") && s("box_after") == "checked", "{out}");
        assert_eq!(out["changed"], true);
        assert!(s("sensitive").starts_with("refused:") && s("sensitive").contains("sensitive"), "{out}");
        assert!(s("approved").starts_with("clicked") && out["order_text"] == true, "{out}");
        assert!(s("secure").starts_with("refused:") && s("secure").contains("secure"));
        assert_eq!(s("typed"), "typed 5 characters into search field 'Search products'");
        assert!(s("pressed") == "pressed return" && out["results"] == true, "{out}");
        assert_eq!(s("title"), "Results - Eve Shop");
        lane.close().await.unwrap();
    });
}

fn shop_plan(page_url: &str) -> Value {
    json!({"steps": [
        {"kind": "open_url", "target": page_url, "expect": {"kind": "title_contains", "value": "Eve Shop"}},
        {"kind": "click", "target": "the Email me deals checkbox", "label_hint": "Email me deals",
         "expect": {"kind": "text_visible", "value": "Email me deals"}},
        {"kind": "type", "target": "Search products", "text": "ramen", "expect": {"kind": "text_visible", "value": "ramen"}},
        {"kind": "press_key", "key": "return", "expect": {"kind": "text_visible", "value": "Results for ramen"}}],
        "final_say": "Searched for ramen.", "success": {"kind": "title_contains", "value": "Results"}})
}

#[test]
#[ignore = "live: launches Playwright's headless Chromium"]
fn test_a_plan_runs_through_the_real_executor_with_no_model() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("shop.html"), PAGE).unwrap();
    let served = format!("{}/shop.html", serve(tmp.path()));
    let page_url = served.replace("http://", "https://"); // the contract insists; the lane is told
    let lane = live_lane(tmp.path(), None, &[(&page_url, &served)]);
    let goal = "on the shop site tick email me deals and search for ramen";
    let result = run(closing(&lane, lane.run_plan(shop_plan(&page_url), goal, 0, no_approvals(), false))).unwrap();
    assert_eq!(result["status"], "complete", "{result}");
    assert_eq!(result["sentence"], "Searched for ramen.");
    let l = &result["ledger"];
    assert!(l[0]["effect"] == "confirmed" && l[0]["how"] == "helper", "{result}");
    assert!(l[1]["effect"] == "confirmed" && l[1]["how"] == "keyword", "{result}");
    assert_eq!(l[3]["effect"], "confirmed");
    assert!(result["ms"].as_f64().unwrap() < 15000.0);
}

#[test]
#[ignore = "live: launches Playwright's headless Chromium"]
fn test_a_sensitive_control_stops_the_plan_for_the_human_and_a_yes_lets_it_through() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("shop.html"), PAGE).unwrap();
    let served = format!("{}/shop.html", serve(tmp.path()));
    let page_url = served.replace("http://", "https://");
    let lane = live_lane(tmp.path(), None, &[(&page_url, &served)]);
    let plan = json!({"steps": [{"kind": "open_url", "target": page_url},
                                {"kind": "click", "target": "the Place Order button", "label_hint": "Place Order",
                                 "expect": {"kind": "text_visible", "value": "Order placed"}}],
                      "final_say": "Ordered.", "success": null});
    let (first, second) = run(closing(&lane, async {
        let goal = "place the order on the shop site";
        let first = lane.run_plan(plan.clone(), goal, 0, no_approvals(), false).await.unwrap();
        let next = first["next_index"].as_u64().unwrap() as usize;
        let mut approved = IndexMap::new();
        approved.insert(next.to_string(), first["confirm"].as_str().unwrap().to_string());
        let second = lane.run_plan(plan, goal, next, approved, false).await.unwrap();
        (first, second)
    }));
    assert_eq!(first["status"], "needs_human", "{first}");
    assert_eq!(eve_desktop_lanes::casefold(first["confirm"].as_str().unwrap()), "place order");
    assert!(!first["ledger"].as_array().unwrap().iter().any(|e| e["effect"] == "confirmed"
        && e["step"].as_str().unwrap().contains("Order")));
    assert!(second["status"] == "complete" && second["sentence"] == "Ordered.", "{second}");
}

const BELOW_THE_FOLD: &str = r##"<!doctype html><html><head><title>Pricing</title></head><body>
<nav><a href="#top">Home</a></nav><h1>Pricing</h1><div style="height:3000px">Features and more features.</div>
<button onclick="document.getElementById('out').textContent='Starter $5, Team $15'">Compare all plans</button>
<p id="out"></p></body></html>"##;

#[test]
#[ignore = "live: launches Playwright's headless Chromium"]
fn test_a_control_below_the_fold_is_collected_and_scrolled_to_before_the_click() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("pricing.html"), BELOW_THE_FOLD).unwrap();
    let url = format!("{}/pricing.html", serve(tmp.path()));
    let lane = live_lane(tmp.path(), None, &[]);
    let plan = json!({"steps": [{"kind": "click", "target": "the Compare all plans button", "label_hint": "Compare all plans",
                                 "expect": {"kind": "text_visible", "value": "Starter $5"}}],
                      "final_say": "Compared.", "success": null});
    let (outline, result) = run(closing(&lane, async {
        lane.open_url(&url).await.unwrap();
        let outline = lane.outline().await.unwrap();
        let result =
            lane.run_plan(plan, "click the Compare all plans button at the bottom", 0, no_approvals(), false).await.unwrap();
        (outline, result)
    }));
    assert!(outline.lines.contains(&"button: Compare all plans".to_string())); // the planner can see it
    assert!(result["status"] == "complete" && result["ledger"][0]["effect"] == "confirmed", "{result}");
}

fn recipes(buttons: &str) -> String {
    format!(
        r##"<!doctype html><html><head><title>Recipes</title></head><body><h1>Recipes</h1>
<ul><li><a href="#apple" onclick="document.title='Apple Crumble'">Apple Crumble</a></li>
<li><a href="#lemon" onclick="document.title='Lemon Tart'">Lemon Tart</a></li></ul>
<div role="dialog" style="display:none">A hidden dialog is not an open one.</div>
<div id="cookie" role="dialog" aria-label="Cookie consent" style="position:fixed;inset:0;background:rgba(0,0,0,.5)">
<div style="background:#fff;padding:24px"><p>We use cookies to improve your experience.</p>{buttons}</div></div></body></html>"##
    )
}

const REJECT: &str = "<button onclick=\"document.getElementById('cookie').remove(); window.choice='reject'\">Reject non-essential cookies</button><button onclick=\"document.getElementById('cookie').remove(); window.choice='accept'\">Accept all cookies</button>";
const ACCEPT_ONLY: &str = "<button onclick=\"document.getElementById('cookie').remove(); window.choice='accept'\">Accept all cookies</button>";

/// `bool(page.evaluate(js, arg))` on the lane's page.
async fn check(lane: &bl::BrowserLane, js: &'static str, arg: Value) -> bool {
    let got = lane.run(move |c| c.ensure().and_then(|p| p.page.evaluate(js, Some(&arg)))).await.unwrap().unwrap();
    eve_vault::pystr::truthy(&got)
}

async fn page_eval(lane: &bl::BrowserLane, script: &'static str) -> Value {
    lane.run(move |c| c.ensure().and_then(|p| p.page.evaluate(script, None))).await.unwrap().unwrap()
}

#[test]
#[ignore = "live: launches Playwright's headless Chromium"]
fn test_a_cookie_notice_over_the_page_is_refused_before_the_plan_clicks() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("recipes.html"), recipes(REJECT)).unwrap();
    let url = format!("{}/recipes.html", serve(tmp.path()));
    let lane = live_lane(tmp.path(), None, &[]);
    let plan = json!({"steps": [{"kind": "click", "target": "the Lemon Tart link", "label_hint": "Lemon Tart",
                                 "expect": {"kind": "title_contains", "value": "Lemon Tart"}}],
                      "final_say": "Opened.", "success": null});
    let (outline, result, choice) = run(closing(&lane, async {
        lane.open_url(&url).await.unwrap();
        let result = lane.run_plan(plan, "open the Lemon Tart recipe", 0, no_approvals(), false).await.unwrap();
        let outline = lane.outline().await.unwrap();
        let choice = page_eval(&lane, "() => window.choice").await;
        (outline, result, choice)
    }));
    assert_eq!(result["status"], "complete", "{result}");
    assert!(result["ledger"][0]["step"] == "dismiss the cookie notice" && choice == "reject", "{result} {choice}");
    assert!(outline.lines.contains(&"link: Lemon Tart".to_string()));
}

#[test]
#[ignore = "live: launches Playwright's headless Chromium"]
fn test_a_notice_that_only_offers_to_accept_stays_up_and_stops_the_plan() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("recipes.html"), recipes(ACCEPT_ONLY)).unwrap();
    let url = format!("{}/recipes.html", serve(tmp.path()));
    let lane = live_lane(tmp.path(), None, &[]);
    let plan = json!({"steps": [{"kind": "click", "target": "the Lemon Tart link", "label_hint": "Lemon Tart"}],
                      "final_say": "Opened.", "success": null});
    let (result, choice) = run(closing(&lane, async {
        lane.open_url(&url).await.unwrap();
        let result = lane.run_plan(plan, "open the Lemon Tart recipe", 0, no_approvals(), false).await.unwrap();
        (result, page_eval(&lane, "() => window.choice || ''").await)
    }));
    assert!(result["status"] == "none" && result["reason"] == "dialog_open" && choice == "", "{result}");
}

const SEARCH_HOME: &str = r#"<!doctype html><html><head><title>Docs</title></head><body><h1>Docs</h1>
<form action="results.html" method="get" role="search"><input type="search" name="q" aria-label="Search docs"></form>
</body></html>"#;
const RESULTS: &str = r##"<!doctype html><html><head><title>Results</title></head><body><a href="#guide">Solar Panel Installation Guide</a></body></html>"##;

#[test]
#[ignore = "live: launches Playwright's headless Chromium"]
fn test_return_in_a_search_field_waits_for_the_results_page() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("results.html"), RESULTS).unwrap();
    std::fs::write(tmp.path().join("docs.html"), SEARCH_HOME).unwrap();
    let url = format!("{}/docs.html", serve(tmp.path()));
    let lane = live_lane(tmp.path(), None, &[]);
    let plan = json!({"steps": [{"kind": "type", "target": "the Search docs field", "label_hint": "Search docs", "text": "solar panels"},
                                {"kind": "press_key", "key": "return"}], "final_say": "Searched.", "success": null});
    let (result, seen) = run(closing(&lane, async {
        lane.open_url(&url).await.unwrap();
        let result = lane.run_plan(plan, "search the docs for solar panels", 0, no_approvals(), false).await.unwrap();
        let seen = lane.run(|c| c.ensure().map(|p| p.page.url())).await.unwrap().unwrap(); // read at once, no extra wait
        (result, seen)
    }));
    assert!(result["status"] == "complete" && seen.contains("results.html?q=solar+panels"), "{result} {seen}");
}

const SECRET_FORM: &str = r#"<!doctype html><html><head><title>Checkout</title></head><body><form>
<input id="pw" type="password">
<input id="shown" type="text" autocomplete="current-password">
<label for="card">Card number</label><input id="card" autocomplete="cc-number">
<label for="cvc">Security code</label><input id="cvc" autocomplete="billing cc-csc">
<label for="otp">Code</label><input id="otp" autocomplete="one-time-code">
<label for="search">Search</label><input id="search" type="search">
</form><script>const v = SECRETS; for (const k in v) document.getElementById(k).value = v[k];</script></body></html>"#;

#[test]
#[ignore = "live: launches Playwright's headless Chromium"]
fn test_what_is_typed_into_a_secret_field_never_reaches_the_snapshot() {
    let secrets = json!({"pw": "hunter2-canary", "shown": "plain-pw-canary", "card": "4242424242424242",
                         "cvc": "987", "otp": "135790", "search": "ramen-control"});
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("checkout.html"), SECRET_FORM.replace("SECRETS", &secrets.to_string())).unwrap();
    let url = format!("{}/checkout.html", serve(tmp.path()));
    let lane = live_lane(tmp.path(), None, &[]);
    let (snap, outline) = run(closing(&lane, async {
        lane.open_url(&url).await.unwrap();
        let snap = lane.run(|c| c.ensure().and_then(|p| p.snapshot())).await.unwrap().unwrap();
        (snap, lane.outline().await.unwrap())
    }));
    let rows: Vec<Value> = snap.elements.iter().map(|c| c.to_dict()).collect();
    let dump = json!({"snap": rows, "outline": outline}).to_string();
    for (key, secret) in secrets.as_object().unwrap() {
        if key != "search" {
            assert!(!dump.contains(secret.as_str().unwrap()), "{key}: {dump}");
        }
    }
    let by = |label: &str| snap.elements.iter().find(|c| c.label == label).unwrap().clone();
    assert_eq!(by("Search").value, "ramen-control"); // an ordinary field is still read
    assert!(by("Card number").value.is_empty() && !by("Card number").secure); // still typeable
    assert_eq!(snap.elements.iter().filter(|c| c.secure).count(), 2, "{dump}"); // the password and the shown password
}

// ---- live: navigation freshness and the URL oracle (test_browser_navigation_freshness.py,
// test_plan_url_expectation.py) ------------------------------------------------------------------------------

/// The lane's page with two instruments: it counts clicks, and injects one navigation race into the first
/// collect after a click.
struct Racing {
    inner: Box<dyn PageHandle>,
    clicks: Arc<AtomicUsize>,
    races: Arc<AtomicUsize>,
}

impl PageHandle for Racing {
    fn evaluate(&self, script: &str, arg: Option<&Value>) -> anyhow::Result<Value> {
        if script == bl::COLLECT_JS && self.clicks.load(Ordering::SeqCst) > 0 && self.races.load(Ordering::SeqCst) == 0 {
            self.races.fetch_add(1, Ordering::SeqCst);
            return Err(eve_desktop_lanes::LaneError::err(
                "Error",
                "Execution context was destroyed, most likely because of a navigation",
            ));
        }
        self.inner.evaluate(script, arg)
    }
    fn title(&self) -> anyhow::Result<String> {
        self.inner.title()
    }
    fn url(&self) -> String {
        self.inner.url()
    }
    fn inner_text(&self, s: &str, t: u64) -> anyhow::Result<String> {
        self.inner.inner_text(s, t)
    }
    fn count(&self, s: &str) -> anyhow::Result<usize> {
        self.inner.count(s)
    }
    fn scroll_into_view_if_needed(&self, s: &str, t: u64) -> anyhow::Result<()> {
        self.inner.scroll_into_view_if_needed(s, t)
    }
    fn click(&self, s: &str, t: u64) -> anyhow::Result<()> {
        self.clicks.fetch_add(1, Ordering::SeqCst);
        self.inner.click(s, t)
    }
    fn fill(&self, s: &str, v: &str, t: u64) -> anyhow::Result<()> {
        self.inner.fill(s, v, t)
    }
    fn select_option(&self, s: &str, c: SelectBy<'_>, t: u64) -> anyhow::Result<()> {
        self.inner.select_option(s, c, t)
    }
    fn evaluate_on(&self, s: &str, script: &str) -> anyhow::Result<Value> {
        self.inner.evaluate_on(s, script)
    }
    fn keyboard_type(&self, t: &str, d: u64) -> anyhow::Result<()> {
        self.inner.keyboard_type(t, d)
    }
    fn keyboard_press(&self, k: &str) -> anyhow::Result<()> {
        self.inner.keyboard_press(k)
    }
    fn wait_for_timeout(&self, ms: u64) {
        self.inner.wait_for_timeout(ms)
    }
    fn wait_for_load_state(&self, s: &str, t: u64) -> anyhow::Result<()> {
        self.inner.wait_for_load_state(s, t)
    }
    fn goto(&self, u: &str, w: &str, t: u64) -> anyhow::Result<()> {
        self.inner.goto(u, w, t)
    }
    fn bring_to_front(&self) -> anyhow::Result<()> {
        self.inner.bring_to_front()
    }
    fn close(&self) -> anyhow::Result<()> {
        self.inner.close()
    }
    fn screenshot_jpeg(&self, q: u8, m: Option<(&str, &str)>) -> anyhow::Result<Vec<u8>> {
        self.inner.screenshot_jpeg(q, m)
    }
}

/// Swap the lane's page for a Racing one over the same tab; returns (clicks, races).
async fn instrument(lane: &bl::BrowserLane) -> (Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let (clicks, races) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (c2, r2) = (clicks.clone(), races.clone());
    lane.run(move |c| {
        c.ensure().unwrap();
        let old = c.page.take().unwrap();
        let racing = Racing { inner: old.page, clicks: c2, races: r2 };
        c.page = Some(bl::Page::new(Box::new(racing)).with_webmcp_probe(true));
    })
    .await
    .unwrap();
    (clicks, races)
}

#[test]
#[ignore = "live: launches Playwright's headless Chromium"]
fn test_navigation_uses_actual_destination_and_never_repeats_click() {
    for target in ["destination", "wrong", "noop"] {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("destination.html"), "<title>Destination evidence</title><h1>Fresh destination body</h1>").unwrap();
        std::fs::write(tmp.path().join("wrong.html"), "<title>Unrelated page</title><h1>Unrelated body</h1>").unwrap();
        let action = if target == "noop" { "event.preventDefault()" } else { "" };
        std::fs::write(
            tmp.path().join("home.html"),
            format!(r#"<title>Home</title><a href="{target}.html" onclick="{action}">Open destination</a><button>Other action</button>"#),
        )
        .unwrap();
        let lane = live_lane(tmp.path(), None, &[]);
        let plan = json!({"steps": [{"kind": "click", "target": "Open destination", "label_hint": "Open destination",
                                     "expect": {"kind": "title_contains", "value": "Destination evidence"}}],
                          "success": {"kind": "text_visible", "value": "Fresh destination body"}, "final_say": "Opened."});
        let home = file_uri(&tmp.path().join("home.html"));
        let (result, url, clicks, races) = run(closing(&lane, async {
            lane.open_url(&home).await.unwrap();
            let (clicks, races) = instrument(&lane).await;
            let result = lane.run_plan(plan, "open destination", 0, no_approvals(), false).await.unwrap();
            let url = lane.run(|c| c.ensure().map(|p| p.page.url())).await.unwrap().unwrap();
            (result, url, clicks.load(Ordering::SeqCst), races.load(Ordering::SeqCst))
        }));
        assert!(clicks == 1 && races == 1, "{target}: clicks {clicks} races {races} {result}");
        if target == "destination" {
            assert!(result["status"] == "complete" && result["sentence"] == "Opened.", "{result}");
            assert!(url.ends_with("destination.html"));
            assert_eq!(result["ledger"][0]["effect"], "confirmed");
        } else {
            assert!(result["status"] == "partial" && result["reason"].as_str().unwrap().contains("did not see it"), "{target}: {result}");
            assert!(url.ends_with(if target == "noop" { "home.html" } else { "wrong.html" }), "{url}");
        }
    }
}

#[test]
#[ignore = "live: launches Playwright's headless Chromium"]
fn test_real_browser_url_oracle_rejects_wrong_and_absent_navigation() {
    for destination in ["destination", "wrong", "noop"] {
        let tmp = tempfile::tempdir().unwrap();
        let href = if destination == "noop" { "/destination".to_string() } else { format!("/{destination}") };
        let click = if destination == "noop" { "event.preventDefault()" } else { "" };
        std::fs::write(
            tmp.path().join("home"),
            format!(r#"<title>Home</title><a href="{href}" onclick="{click}">Destination</a><button>Other action</button>"#),
        )
        .unwrap();
        for name in ["destination", "wrong"] {
            std::fs::write(tmp.path().join(name), "<title>Generic destination</title><p>Generic contents</p>").unwrap();
        }
        let base = serve(tmp.path());
        let want = format!("{base}/destination");
        let lane = live_lane(tmp.path(), None, &[]);
        let plan = json!({"steps": [{"kind": "click", "target": "Destination", "label_hint": "Destination",
                                     "expect": {"kind": "url_is", "value": want}}],
                          "success": {"kind": "url_is", "value": want}, "final_say": "Opened."});
        let (result, url, clicks) = run(closing(&lane, async {
            lane.open_url(&format!("{base}/home")).await.unwrap();
            let (clicks, _) = instrument(&lane).await;
            let result = lane.run_plan(plan, "open Destination", 0, no_approvals(), false).await.unwrap();
            let url = lane.run(|c| c.ensure().map(|p| p.current_url())).await.unwrap().unwrap();
            (result, url, clicks.load(Ordering::SeqCst))
        }));
        assert_eq!(clicks, 1, "{result}");
        if destination == "destination" {
            assert!(result["status"] == "complete" && result["ledger"][0]["effect"] == "confirmed", "{result}");
            assert_eq!(url, want);
        } else {
            assert!(result["status"] == "partial" && result["reason"].as_str().unwrap().contains("did not see it"), "{result}");
            assert_eq!(url, format!("{base}/{}", if destination == "noop" { "home" } else { "wrong" }));
        }
    }
}

#[test]
#[ignore = "live: launches Playwright's headless Chromium"]
fn test_actual_relative_href_is_resolved_and_ambiguous_href_is_omitted() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("home"),
        r##"<title>Metadata</title><a href="/guide">Guide</a>
        <a href="?q=1">Query only</a><a href="#section">Hash only</a>
        <a href="https://owner:password@other.test/guide">Credentials</a>
        <a href="javascript:void(0)">Script</a><a href="https://other.test/guide">Other origin</a>"##,
    )
    .unwrap();
    let base = serve(tmp.path());
    let lane = live_lane(tmp.path(), None, &[]);
    let (ordinary, ambiguous, observed) = run(closing(&lane, async {
        lane.open_url(&format!("{base}/home")).await.unwrap();
        let ordinary = lane.outline().await.unwrap();
        lane.open_url(&format!("{base}/home?q=1#section")).await.unwrap();
        let ambiguous = lane.outline().await.unwrap();
        let observed = lane.run(|c| c.ensure().map(|p| p.current_url())).await.unwrap().unwrap();
        (ordinary, ambiguous, observed)
    }));
    assert_eq!(ordinary.url, format!("{base}/home"));
    assert_eq!(ordinary.site, "127.0.0.1");
    assert!(ordinary.lines.contains(&format!("link: Guide; destination: {base}/guide")), "{:?}", ordinary.lines);
    assert!(ordinary.lines.contains(&"link: Other origin; destination: https://other.test/guide".to_string()));
    for label in ["Query only", "Hash only", "Credentials", "Script"] {
        assert!(ordinary.lines.contains(&format!("link: {label}")), "{label}: {:?}", ordinary.lines);
    }
    assert!(ambiguous.url.is_empty() && observed == format!("{base}/home?q=1#section"));
    assert_eq!(ambiguous.site, "127.0.0.1");
}

#[test]
#[ignore = "live: launches Playwright's headless Chromium"]
fn test_real_chromium_sees_registered_tools_and_their_absence() {
    let tmp = tempfile::tempdir().unwrap();
    let shim = "<script>Object.defineProperty(document, 'modelContext', {value: {\n        getTools: async () => [{name: 'search_flights'}, {name: 'book'}]}});</script>";
    std::fs::write(tmp.path().join("tools"), format!("<title>Tools</title>{shim}<p>hi</p>")).unwrap();
    std::fs::write(tmp.path().join("plain"), "<title>Plain</title><p>hi</p>").unwrap();
    let base = serve(tmp.path());
    let lane = live_lane(tmp.path(), None, &[]);
    let lines = run(closing(&lane, async {
        let mut lines = vec![];
        for page in ["tools", "plain"] {
            lane.open_url(&format!("{base}/{page}")).await.unwrap();
            // the lane logs on its own thread: capture there
            let (outline, logged) = lane.run(|c| capture_logs(|| c.outline())).await.unwrap();
            outline.unwrap();
            lines.extend(logged.into_iter().filter(|l| l.contains("webmcp probe")));
        }
        lines
    }));
    assert_eq!(
        lines,
        [
            "browser lane: webmcp probe host=127.0.0.1 api=True tools=2 names=search_flights,book",
            "browser lane: webmcp probe host=127.0.0.1 api=False tools=- names=-",
        ]
    );
}

// ---- live: a stand-in Chrome, never the owner's (test_browser_attach.py) --------------------------------------

#[test]
#[ignore = "live: launches Playwright's Chromium as a stand-in for the owner's Chrome"]
fn test_live_attach_to_a_standin_chrome_works_in_its_own_tab_and_leaves_the_owners() {
    let Some(exe) = bl::cdp::find_chromium(false) else {
        eprintln!("skipped: Playwright's Chromium is not installed");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let page = tmp.path().join("owner.html");
    std::fs::write(&page, "<title>owner</title><input aria-label='Search here' type='search'>").unwrap();
    let ud = tmp.path().join("ud");
    let mut proc = std::process::Command::new(exe)
        .args(["--headless=new", "--remote-debugging-port=0", &format!("--user-data-dir={}", ud.display()), "--no-first-run"])
        .arg(file_uri(&page))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let port_file = ud.join("DevToolsActivePort");
    for _ in 0..60 {
        if port_file.exists() && bl::devtools_endpoint(&ud, bl::DEFAULT_DEBUG_PORT).is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let port = std::fs::read_to_string(&port_file).unwrap().split_whitespace().next().unwrap().to_string();
    let tabs = || -> Vec<String> {
        // plain HTTP over a socket: this runs inside the async test, where a blocking client may not
        let mut sock = std::net::TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
        sock.write_all(format!("GET /json/list HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n").as_bytes())
            .unwrap();
        // Chrome keeps the socket open: read the headers, then Content-Length bytes
        let mut reply = Vec::new();
        let mut byte = [0u8; 1];
        while !reply.ends_with(b"\r\n\r\n") {
            sock.read_exact(&mut byte).unwrap();
            reply.push(byte[0]);
        }
        let head = String::from_utf8_lossy(&reply).to_lowercase();
        let len: usize = head.split("content-length:").nth(1).unwrap().split("\r\n").next().unwrap().trim().parse().unwrap();
        let mut body = vec![0u8; len];
        sock.read_exact(&mut body).unwrap();
        let body = String::from_utf8(body).unwrap();
        let list: Vec<Value> = serde_json::from_str(&body).unwrap();
        let mut urls: Vec<String> =
            list.iter().filter(|t| t["type"] == "page").map(|t| t["url"].as_str().unwrap().to_string()).collect();
        urls.sort();
        urls
    };
    let owners = tabs();
    let cfg = bl::BrowserLaneConfig { attach: true, chrome_dir: ud.clone(), ..Default::default() };
    let mut core = bl::LaneCore::new(cfg, options()).unwrap();
    core.seams.ensure_chrome_window = Arc::new(|| Ok(true)); // the stand-in is headless: no osascript
    let lane = bl::BrowserLane::from_core(core).unwrap();
    let said = run(closing(&lane, async {
        lane.open_url(&file_uri(&page)).await.unwrap();
        assert_eq!(tabs().len(), owners.len() + 1); // eve's own new tab
        lane.run(|c| -> anyhow::Result<String> {
            let p = c.ensure()?;
            let boxed = p.snapshot()?.elements.into_iter().find(|c| c.editable).unwrap();
            Ok(p.focus_and_type(&boxed, "hello"))
        })
        .await
        .unwrap()
        .unwrap()
    }));
    assert!(said.starts_with("typed 5 characters"), "{said}");
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(tabs(), owners); // the owner's tabs, untouched
    assert!(proc.try_wait().unwrap().is_none()); // and their browser still running
    let _ = proc.kill();
    let _ = proc.wait();
}

// ---- live: the vault fill and the masked screenshot (test_vault_fill.py) --------------------------------------

const LOGIN: &str = r#"<!doctype html><html><head><title>Sign in</title></head><body><form id=f onsubmit="event.preventDefault()">
<input id=user type=text><input id=pw type=password><button>Sign in</button></form></body></html>"#;
const CARD: &str = r#"<!doctype html><html><head><title>Checkout</title></head><body><form>
<label for=n>Card number</label><input id=n autocomplete="cc-number">
<label for=e>Expiry</label><input id=e autocomplete="cc-exp">
<label for=c>CVC</label><input id=c autocomplete="cc-csc">
<label for=h>Name on card</label><input id=h autocomplete="cc-name"></form></body></html>"#;
const OTP: &str = r#"<!doctype html><html><head><title>Verify</title></head><body><form>
<label for=o>Enter the code from your app</label><input id=o autocomplete="one-time-code" inputmode=numeric></form></body></html>"#;

fn card_plain() -> String {
    let months: String = (1..=12).map(|m| format!("<option value='{m:02}'>{m:02}</option>")).collect();
    let years: String = (2026..2036).map(|y| format!("<option>{y}</option>")).collect();
    format!(
        r#"<!doctype html><html><head><title>Pay</title></head><body><form>
<label for=n>Card number</label><input id=n name=cardnumber>
<label for=m>Expiry month</label><select id=m name=exp-month><option value="">MM</option>{months}</select>
<label for=y>Expiry year</label><select id=y name=exp-year><option value="">YYYY</option>{years}</select>
<label for=c>Security code</label><input id=c name=cvv></form></body></html>"#
    )
}

fn fill_plan(handle: &str, target: &str) -> Value {
    json!({"steps": [{"kind": "fill", "target": target, "label_hint": "", "text": handle, "key": "",
                      "expect": null, "consequential": false}], "final_say": "Filled.", "success": null})
}

fn pixel_mean(jpeg: &[u8], b: &Value) -> [f64; 3] {
    let img = image::load_from_memory(jpeg).unwrap().to_rgb8();
    let f = |k: &str| b[k].as_f64().unwrap();
    let (x0, y0) = ((f("x") + 3.0) as u32, (f("y") + 3.0) as u32);
    let (x1, y1) = ((f("x") + f("width") - 3.0) as u32, (f("y") + f("height") - 3.0) as u32);
    let (mut sum, mut n) = ([0f64; 3], 0f64);
    for x in (x0..x1).step_by(2) {
        for y in (y0..y1).step_by(2) {
            let p = img.get_pixel(x, y);
            for i in 0..3 {
                sum[i] += p[i] as f64;
            }
            n += 1.0;
        }
    }
    sum.map(|s| s / n)
}

#[test]
#[ignore = "live: launches Playwright's headless Chromium"]
fn test_live_fill_fills_each_form_and_no_value_leaves_the_page() {
    let canaries = [("username", "canary-user@example.com"), ("password", "canary-pw-9f3!"), ("number", "4000056655665556"),
                    ("cvc", "713"), ("name", "Canary Holder")];
    let canary = |k: &str| canaries.iter().find(|(n, _)| *n == k).unwrap().1;
    let tmp = tempfile::tempdir().unwrap();
    for (name, html) in [("login.html", LOGIN.to_string()), ("card.html", CARD.into()), ("pay.html", card_plain()), ("otp.html", OTP.into())] {
        std::fs::write(tmp.path().join(name), html).unwrap();
    }
    let base = serve(tmp.path());
    let v = Arc::new(eve_vault::Vault::with_clock(tmp.path().join("vault.json"), Box::new(eve_vault::MemKeychain::new()), || 1_234_567_890.0));
    let login = v.add("login", "Local", &values(&[("username", canary("username")), ("password", canary("password"))]), &base).unwrap();
    let elsewhere = v.add("login", "Elsewhere", &values(&[("username", "x"), ("password", "elsewhere-pw")]), "other.example").unwrap();
    let card = v
        .add("card", "Visa", &values(&[("number", canary("number")), ("exp_month", "7"), ("exp_year", "2031"),
                                     ("cvc", canary("cvc")), ("name", canary("name"))]), "")
        .unwrap();
    let secret_b32 = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";
    let code = v.add("totp", "Local 2FA", &values(&[("secret", secret_b32)]), &base).unwrap();
    let want_code = eve_vault::totp_default(secret_b32, Some(1_234_567_890.0)).unwrap();
    let lane = live_lane(tmp.path(), Some(v.clone()), &[]);
    let mut seen: Vec<Value> = vec![];
    let out = run(closing(&lane, async {
        let mut out = serde_json::Map::new();
        lane.open_url(&format!("{base}/login.html")).await.unwrap();
        let outline = lane.outline().await.unwrap();
        seen.push(json!(outline));
        let refused = lane.run_plan(fill_plan(&elsewhere.handle, "the sign-in form"), "sign me in", 0, no_approvals(), false).await.unwrap();
        seen.push(refused.clone());
        out.insert("refused".into(), refused);
        out.insert("empty_after_refusal".into(), json!(check(&lane, "() => !document.getElementById('user').value && !document.getElementById('pw').value", Value::Null).await));
        let result = lane.run_plan(fill_plan(&login.handle, "the sign-in form"), "sign me in", 0, no_approvals(), false).await.unwrap();
        seen.push(result.clone());
        out.insert("login".into(), result);
        out.insert("login_ok".into(), json!(check(&lane, "w => document.getElementById('user').value === w[0] && document.getElementById('pw').value === w[1]",
                                                 json!([canary("username"), canary("password")])).await));
        let snap = lane.run(|c| c.ensure().and_then(|p| p.snapshot())).await.unwrap().unwrap();
        seen.push(json!(snap.elements.iter().map(|c| c.to_dict()).collect::<Vec<_>>()));
        seen.push(json!(lane.outline().await.unwrap()));
        let bbox = |sel: &'static str| {
            let js = match sel {
                "#pw" => "() => { const r = document.querySelector('#pw').getBoundingClientRect(); return {x: r.x, y: r.y, width: r.width, height: r.height}; }",
                _ => "() => { const r = document.querySelector('#user').getBoundingClientRect(); return {x: r.x, y: r.y, width: r.width, height: r.height}; }",
            };
            page_eval(&lane, js)
        };
        let pw_box = bbox("#pw").await;
        let masked = lane.screenshot().await.unwrap().unwrap();
        let plain = lane.run(|c| c.ensure().and_then(|p| p.page.screenshot_jpeg(85, None))).await.unwrap().unwrap();
        out.insert("masked".into(), json!(pixel_mean(&masked, &pw_box)));
        out.insert("plain".into(), json!(pixel_mean(&plain, &pw_box)));
        let user_box = bbox("#user").await;
        out.insert("user_masked".into(), json!(pixel_mean(&masked, &user_box)));

        lane.open_url(&format!("{base}/card.html")).await.unwrap();
        let result = lane.run_plan(fill_plan(&card.handle, "the card form"), "pay with my visa", 0, no_approvals(), false).await.unwrap();
        seen.push(result.clone());
        out.insert("card".into(), result);
        out.insert("card_ok".into(), json!(check(&lane, "w => ['n','e','c','h'].every((id, i) => document.getElementById(id).value === w[i])",
                                                json!([canary("number"), "07/31", canary("cvc"), canary("name")])).await));
        seen.push(json!(lane.outline().await.unwrap()));

        lane.open_url(&format!("{base}/pay.html")).await.unwrap();
        let result = lane.run_plan(fill_plan(&card.handle, "the payment form"), "pay with my visa", 0, no_approvals(), false).await.unwrap();
        seen.push(result.clone());
        out.insert("pay".into(), result);
        out.insert("pay_ok".into(), json!(check(&lane, "w => ['n','m','y','c'].every((id, i) => document.getElementById(id).value === w[i])",
                                               json!([canary("number"), "07", "2031", canary("cvc")])).await));
        seen.push(json!(lane.outline().await.unwrap()));

        lane.open_url(&format!("{base}/otp.html")).await.unwrap();
        let result = lane.run_plan(fill_plan(&code.handle, "the code field"), "enter my 2fa code", 0, no_approvals(), false).await.unwrap();
        seen.push(result.clone());
        out.insert("otp_ok".into(), json!(check(&lane, "w => document.getElementById('o').value === w", json!(want_code)).await));
        seen.push(json!(lane.outline().await.unwrap()));
        Value::Object(out)
    }));
    assert!(out["refused"]["status"] == "none" && out["refused"].to_string().contains("saved for other.example"), "{}", out["refused"]);
    assert_eq!(out["empty_after_refusal"], true);
    assert_eq!(out["login"]["status"], "complete", "{}", out["login"]);
    for k in ["login_ok", "card_ok", "pay_ok", "otp_ok"] {
        assert_eq!(out[k], true, "{k}: {out}");
    }
    assert!(out["card"]["status"] == "complete" && out["pay"]["status"] == "complete", "{} {}", out["card"], out["pay"]);
    let dump = Value::Array(seen.clone()).to_string();
    for (name, value) in canaries.iter().copied().chain([("code", want_code.as_str()), ("elsewhere", "elsewhere-pw")]) {
        assert!(!dump.contains(value), "{name}");
    }
    let vault_lines = seen[0]["vault"].as_array().unwrap();
    assert!(vault_lines.iter().any(|l| l.as_str().unwrap().starts_with(&format!("{} login \"Local\" for 127.0.0.1", login.handle))), "{vault_lines:?}");
    let close = |a: &Value, b: [f64; 3]| (0..3).all(|i| (a[i].as_f64().unwrap() - b[i]).abs() < 24.0); // JPEG noise
    let mask = [0.0, 0.0, 0.0]; // MASK_COLOR #000000
    assert!(close(&out["masked"], mask) && close(&out["user_masked"], mask), "{}", out["masked"]);
    assert!(!close(&out["plain"], mask), "{}", out["plain"]); // positive control: unmasked differs
}

// ---- live: local Steel (test_steel_local.py) -----------------------------------------------------------------

#[test]
#[ignore = "live: needs the local Steel server checkout on this Mac"]
fn test_live_the_lane_reads_a_page_through_local_steel_starting_it_when_down() {
    let lane = bl::BrowserLane::new(bl::BrowserLaneConfig { attach: true, steel: true, ..Default::default() }, options()).unwrap();
    let out = run(closing(&lane, async {
        lane.connect().await.unwrap();
        lane.open_url("https://example.com").await.unwrap();
        lane.outline().await.unwrap()
    }));
    assert!(out.title == "Example Domain" && out.site == "example.com");
}
