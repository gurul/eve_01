//! Port of bridge/tests/test_app_check.py's browser tests, and of the tests in bridge/tests/test_jev_verify.py
//! that walk journeys through `check_app`.
//!
//! These run real headless Chromium against handwritten apps, a correct one and one for each way an app breaks,
//! so a finding is proven to fire on the bug and, as important, not to fire on a correct app: a false alarm costs
//! a repair round that can make a working app worse. No network: the fixtures load nothing remote. Without a
//! Chromium (Playwright's, or an installed Chrome) every test here returns at once, as Python's importorskip.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use eve_app_check::*;
use eve_jev_verify::{self as jv, Journey, Predict, Verifier, WalkOptions};
use futures::FutureExt;
use parking_lot::Mutex;
use serde_json::{json, Value};

/// At most this many Chromiums at once, so the timing assertions hold on a busy machine.
static GATE: LazyLock<tokio::sync::Semaphore> = LazyLock::new(|| tokio::sync::Semaphore::new(3));

fn have_chromium() -> bool {
    let found = cdp::find_chromium().is_some();
    if !found {
        eprintln!("no Chromium: skipped");
    }
    found
}

async fn run_with(html: &str, opts: CheckOptions) -> CheckReport {
    let _permit = GATE.acquire().await.unwrap();
    check_app(html, opts).await
}

async fn run(html: &str) -> CheckReport {
    run_with(html, CheckOptions { verifier: VerifierChoice::None, ..Default::default() }).await
}

fn head(title: &str, style: &str) -> String {
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
<title>{title}</title>
<style>
  body {{ margin: 0; padding: 16px; font: 16px -apple-system, system-ui, sans-serif;
         background: var(--tg-theme-bg-color, #fff); color: var(--tg-theme-text-color, #000); }}
  button, input {{ min-height: 44px; font: inherit; }}
  li {{ display: flex; gap: 8px; align-items: center; }}
  {style}
</style></head><body>"#
    )
}

/// A small, correct habit tracker: loads once, renders, saves after every change, confirms deletes natively.
const GOOD_BODY: &str = r#"
<form id="f"><input id="name" placeholder="New habit" aria-label="Habit name" required>
<button type="submit">Add</button></form>
<ul id="list"></ul>
<p id="empty">No habits yet. Try "Drink water".</p>
<script>
const tg = window.Telegram.WebApp; tg.ready(); tg.expand();
const today = () => { const d = new Date(); return d.getFullYear() + "-" + String(d.getMonth() + 1).padStart(2, "0")
  + "-" + String(d.getDate()).padStart(2, "0"); };
let state = { v: 1, habits: [] };
async function persist() { try { await eve.save(state); } catch (e) { tg.showAlert("Could not save"); } }
function render() {
  const list = document.getElementById("list"); list.innerHTML = "";
  document.getElementById("empty").hidden = state.habits.length > 0;
  for (const h of state.habits) {
    const li = document.createElement("li");
    const done = h.days.includes(today());
    li.innerHTML = `<button class="tick" aria-label="Check in ${h.name}">${done ? "✓" : "○"}</button>
      <span>${h.name} · ${h.days.length} days</span><button class="del" aria-label="Delete ${h.name}">Delete</button>`;
    li.querySelector(".tick").onclick = () => { tg.HapticFeedback.impactOccurred("light");
      h.days = done ? h.days.filter((d) => d !== today()) : [...h.days, today()]; render(); persist(); };
    li.querySelector(".del").onclick = () => tg.showConfirm(`Delete ${h.name}?`, (ok) => {
      if (!ok) return; state.habits = state.habits.filter((x) => x !== h); render(); persist(); });
    list.appendChild(li);
  }
}
document.getElementById("f").addEventListener("submit", (e) => {
  e.preventDefault(); const name = document.getElementById("name").value.trim(); if (!name) return;
  state.habits.push({ id: String(Date.now()), name, days: [] }); document.getElementById("name").value = "";
  render(); persist();
});
(async () => { const saved = await eve.load(); if (saved && saved.v === 1) state = saved; render(); })();
</script></body></html>"#;

fn good() -> String {
    head("Habits", "") + GOOD_BODY
}

fn variant_of(base: &str, old: &str, new: &str) -> String {
    assert!(base.contains(old), "{old}");
    base.replacen(old, new, 1)
}

fn variant(old: &str, new: &str) -> String {
    variant_of(&good(), old, new)
}

fn throws_on_tap() -> String {
    variant("h.days = done ?", "undefinedHelper(h); h.days = done ?")
}

/// Saves a shape its own loader cannot read back: fine on day one, broken on day two.
fn throws_on_reopen() -> String {
    variant(
        "if (saved && saved.v === 1) state = saved;",
        "if (saved) { state = saved; state.habits.forEach((h) => h.streak.toFixed(0)); }",
    )
}

/// Adds only through Telegram's MainButton, the way the prompt suggests for the primary action.
fn main_button() -> String {
    variant(r#"<button type="submit">Add</button>"#, "").replacen(
        "(async () => {",
        "tg.MainButton.setText('Add habit').show().onClick(() => document.getElementById('f').requestSubmit());\n(async () => {",
        1,
    )
}

fn has(issues: &[String], f: impl Fn(&str) -> bool) -> bool {
    issues.iter().any(|i| f(i))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_correct_app_passes_quickly_and_comes_back_with_a_screenshot() {
    if !have_chromium() {
        return;
    }
    let _permit = GATE.acquire().await.unwrap();
    let t0 = Instant::now();
    let r = check_app(&good(), CheckOptions { verifier: VerifierChoice::None, ..Default::default() }).await;
    let took = t0.elapsed().as_secs_f64();
    assert!(r.ok, "{:?}", r.issues);
    assert!(r.fields >= 1 && r.taps >= 2 && r.saves >= 2 && r.skipped.is_empty(), "{r:?}");
    assert_eq!(r.saved["v"], 1); // used, then its delete confirmed and saved
    let shot = r.screenshot.as_ref().unwrap();
    assert_eq!(&shot[..2], b"\xff\xd8"); // a JPEG
    assert!(took < 15.0, "{took}");
    assert!(r.summary().starts_with("passed"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_the_reopen_and_the_screenshot_use_the_data_from_before_the_deletes() {
    if !have_chromium() {
        return;
    }
    // delete runs last; the owner's screenshot shows the app with a habit in it, not the emptied one
    let r = run(&good()).await;
    assert_eq!(r.saved["habits"], json!([])); // the check deleted what it made…
    assert!(r.ok && r.screenshot.as_ref().unwrap().len() > 10_000, "{:?}", r.issues); // …but photographed the app with data
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_an_error_on_tap_names_the_tap_and_the_line() {
    if !have_chromium() {
        return;
    }
    let r = run(&throws_on_tap()).await;
    assert!(!r.ok);
    let hit: Vec<&String> = r.issues.iter().filter(|i| i.contains("undefinedHelper")).collect();
    assert!(!hit.is_empty() && hit[0].contains("tapping") && hit[0].contains("Check in") && hit[0].contains("(line "), "{:?}", r.issues);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_an_app_that_cannot_read_its_own_saved_data_is_caught_on_reopen() {
    if !have_chromium() {
        return;
    }
    let r = run(&throws_on_reopen()).await;
    assert!(has(&r.issues, |i| i.contains("reopening the app") && i.contains("toFixed")), "{:?}", r.issues);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_an_app_that_never_saves_is_caught() {
    if !have_chromium() {
        return;
    }
    let r = run(&variant("async function persist() { try { await eve.save(state); }", "async function persist() { try { }")).await;
    assert!(r.saves == 0 && has(&r.issues, |i| i.contains("never called eve.save")), "{:?}", r.issues);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_an_app_used_only_by_taps_that_never_saves_is_caught() {
    if !have_chromium() {
        return;
    }
    let taps_only = head("Counter", "")
        + r#"<p id="n">0</p><button id="b">Add one</button><script>let n = 0; eve.load().then((d) => { n = (d && d.n) || 0; document.getElementById('n').textContent = n; });document.getElementById('b').onclick = () => { n++; document.getElementById('n').textContent = n; };</script></body></html>"#;
    let r = run(&taps_only).await;
    assert!(has(&r.issues, |i| i.contains("never called eve.save")), "{:?}", r.issues);
    let fixed = run(&taps_only.replace("textContent = n; };", "textContent = n; eve.save({ n }); };")).await;
    assert!(fixed.ok, "{:?}", fixed.issues); // the same app, saving: passes
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_reaching_another_host_is_caught_and_blocked() {
    if !have_chromium() {
        return;
    }
    let r = run(&variant("(async () => {", "fetch('https://api.example.com/habits').catch(() => {});\n(async () => {")).await;
    assert!(has(&r.issues, |i| i.contains("api.example.com")), "{:?}", r.issues);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_iframe_to_another_host_is_fenced_and_reported_too() {
    if !have_chromium() {
        return;
    }
    let r = run(&variant("<ul id=\"list\"></ul>", "<iframe src=\"https://frames.example.org/x\"></iframe><ul id=\"list\"></ul>")).await;
    assert!(has(&r.issues, |i| i.contains("tried to reach frames.example.org")), "{:?}", r.issues);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_page_wider_than_the_phone_is_caught() {
    if !have_chromium() {
        return;
    }
    let too_wide = head("Wide", ".banner { width: 640px; height: 40px; }") + &GOOD_BODY.replacen("<form", r#"<div class="banner"></div><form"#, 1);
    let r = run(&too_wide).await;
    assert!(has(&r.issues, |i| i.contains("wider than a 390px phone") && i.contains("sideways")), "{:?}", r.issues);
    assert!(!has(&r.issues, |i| i.contains("desktop")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_page_without_a_viewport_meta_is_caught() {
    if !have_chromium() {
        return;
    }
    let no_viewport =
        good().replace(r#"<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">"#, "");
    let r = run(&no_viewport).await;
    assert!(has(&r.issues, |i| i.contains("desktop") && i.contains("name=\"viewport\"")), "{:?}", r.issues);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_form_that_reloads_the_page_is_caught() {
    if !have_chromium() {
        return;
    }
    let r = run(&variant("e.preventDefault(); ", "")).await;
    assert!(has(&r.issues, |i| i.contains("reloaded") && i.contains("preventDefault")), "{:?}", r.issues);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_the_main_button_is_pressed_like_a_person_would() {
    if !have_chromium() {
        return;
    }
    let r = run(&main_button()).await;
    assert!(r.ok, "{:?}", r.issues);
    assert!(r.saves >= 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_an_app_that_freezes_is_reported_not_hung() {
    if !have_chromium() {
        return;
    }
    let freezes = variant("(async () => {", "document.addEventListener('click', () => { while (true) {} });\n(async () => {");
    let _permit = GATE.acquire().await.unwrap();
    let t0 = Instant::now();
    let r = check_app(&freezes, CheckOptions { verifier: VerifierChoice::None, check_timeout_s: 8.0, ..Default::default() }).await;
    assert!(t0.elapsed().as_secs_f64() < 20.0);
    assert!(has(&r.issues, |i| i.contains("stopped responding")), "{:?}", r.issues);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_seed_is_what_the_app_opens_with_and_is_never_changed() {
    if !have_chromium() {
        return;
    }
    let seed = json!({"v": 1, "habits": [{"id": "1", "name": "Stretch", "days": []}]});
    // the owner's real data, shape without "streak": caught at once
    let r = run_with(&throws_on_reopen(), CheckOptions { seed: Some(seed.clone()), verifier: VerifierChoice::None, ..Default::default() }).await;
    assert!(has(&r.issues, |i| i.contains("opening the app") && i.contains("toFixed")), "{:?}", r.issues);
    assert_eq!(seed, json!({"v": 1, "habits": [{"id": "1", "name": "Stretch", "days": []}]}));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_no_chromium_is_a_skip_not_a_failure() {
    let r = run_with(
        &good(),
        CheckOptions { executable: Some("/nonexistent/chromium".into()), verifier: VerifierChoice::None, ..Default::default() },
    )
    .await;
    assert!(!r.skipped.is_empty() && r.summary().starts_with("not tested") && r.issues.is_empty(), "{r:?}");
}

// ---- what a person sees: the dark theme, fingers, charts -----------------------------------------------------

/// A small app that loads and saves like a good one, with `body` and `style` to test how it looks.
fn page(body: &str, style: &str) -> String {
    head("Look", style)
        + body
        + "<script>eve.load().then(() => {});document.querySelectorAll('button').forEach((b) => b.addEventListener('click', () => eve.save({ v: 1 })));</script></body></html>"
}

const SEG: &str = r#"<h1>Range</h1><div class="seg"><button aria-selected="true">Today</button><button aria-selected="false">Week</button></div><p>Pick a range.</p>"#;
const SEG_STYLE: &str = ".seg { display: flex; padding: 2px; background: rgba(118, 118, 128, .16); border-radius: 10px; }.seg button { flex: 1; border: 0; background: none; color: var(--tg-theme-text-color); }";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_text_that_vanishes_in_the_dark_theme_is_caught() {
    if !have_chromium() {
        return;
    }
    let r = run(&page("<h2 class='t'>Your streaks</h2>", ".t { color: #222; }")).await;
    assert!(has(&r.issues, |i| i.contains("dark theme") && i.contains("Your streaks")), "{:?}", r.issues);
    let fine = run(&page("<h2 class='t'>Your streaks</h2>", ".t { color: var(--tg-theme-text-color, #222); }")).await;
    assert!(fine.ok, "{:?}", fine.issues);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_selected_segment_that_looks_like_the_others_in_the_dark_is_caught() {
    if !have_chromium() {
        return;
    }
    let faint = format!(
        "{SEG_STYLE}.seg button[aria-selected=true] {{ background: var(--tg-theme-section-bg-color); font-weight: 600; box-shadow: 0 1px 3px rgba(0,0,0,.15); }}"
    );
    let r = run(&page(SEG, &faint)).await;
    assert!(has(&r.issues, |i| i.contains("selected") && i.contains("\"Today\"") && i.contains("\"Week\"")), "{:?}", r.issues);
    let clear = format!(
        "{SEG_STYLE}.seg button[aria-selected=true] {{ background: var(--tg-theme-button-color); color: var(--tg-theme-button-text-color); }}"
    );
    let r = run(&page(SEG, &clear)).await;
    assert!(r.ok, "{:?}", r.issues);
    // a state shown inside the element (a filled dot) is a real difference, not a missing one
    let dots = r#"<h1>Days</h1><div class="seg"><button aria-pressed="true"><span class="d on"></span>Mon</button><button aria-pressed="false"><span class="d"></span>Tue</button></div>"#;
    let r = run(&page(dots, &format!("{SEG_STYLE}.d {{ display: inline-block; width: 8px; height: 8px; }} .d.on {{ background: red; }}"))).await;
    assert!(r.ok, "{:?}", r.issues);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_date_field_left_in_light_colors_in_the_dark_is_caught() {
    if !have_chromium() {
        return;
    }
    let field = r#"<label for="d">Date</label><input id="d" type="date">"#;
    let dark = "input { background: var(--tg-theme-section-bg-color); color: var(--tg-theme-text-color); }";
    let r = run(&page(field, dark)).await;
    assert!(has(&r.issues, |i| i.contains("color-scheme") && i.contains("\"Date\"")), "{:?}", r.issues);
    let r = run(&page(field, &format!("{dark}:root {{ color-scheme: light dark; }}"))).await;
    assert!(r.ok, "{:?}", r.issues);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_tap_targets_too_small_for_a_finger_are_caught_unless_their_hit_area_is_wider() {
    if !have_chromium() {
        return;
    }
    let tiny = r#"<button class="x" aria-label="Remove">×</button>"#;
    let small = ".x { min-height: 0; width: 22px; height: 22px; padding: 0; position: relative; }";
    let r = run(&page(tiny, small)).await;
    assert!(has(&r.issues, |i| i.contains("Tap targets too small") && i.contains("\"Remove\" 22x22px")), "{:?}", r.issues);
    let wider = format!("{small}.x::after {{ content: ''; position: absolute; inset: -12px; }}");
    let r = run(&page(tiny, &wider)).await;
    assert!(r.ok, "{:?}", r.issues);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_inputs_small_enough_to_make_iphone_zoom_are_caught() {
    if !have_chromium() {
        return;
    }
    let r = run(&page(r#"<input aria-label="Habit name" class="s">"#, ".s { font-size: 13px; }")).await;
    assert!(has(&r.issues, |i| i.contains("under 16px") && i.contains("Habit name")), "{:?}", r.issues);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_chart_bar_stretched_by_a_css_class_is_caught() {
    if !have_chromium() {
        return;
    }
    let chart = r#"<svg viewBox="0 0 100 50" width="100%"><rect class="bar today" x="10" y="10" width="10" height="40"/><rect class="bar" x="30" y="20" width="10" height="30"/></svg><section class="today">Today</section>"#;
    let r = run(&page(chart, ".today { display: block; width: 100%; }")).await;
    assert!(has(&r.issues, |i| i.contains("chart shape") && i.contains("class=\"bar today\"")), "{:?}", r.issues);
    let r = run(&page(&chart.replace(r#"class="today""#, r#"class="today-card""#), ".today-card { width: 100%; }")).await;
    assert!(r.ok, "{:?}", r.issues);
}

// ---- how it keeps data, and time -----------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_save_that_only_lands_after_telegram_closed_the_app_is_caught() {
    if !have_chromium() {
        return;
    }
    let debounced = head("Counter", "")
        + r#"<p id="n">0</p><button id="b">Add one</button><script>let n = 0, t; eve.load().then((d) => { n = (d && d.n) || 0; document.getElementById('n').textContent = n; });document.getElementById('b').onclick = () => { n++; document.getElementById('n').textContent = n; clearTimeout(t); t = setTimeout(() => eve.save({ n }), 800); };</script></body></html>"#;
    let r = run(&debounced).await;
    assert!(has(&r.issues, |i| i.contains("only after it was closed")), "{:?}", r.issues);
    let flushed = debounced.replace(
        "</script>",
        "document.addEventListener('visibilitychange', () => {if (document.visibilityState === 'hidden') { clearTimeout(t); eve.save({ n }); } });</script>",
    );
    let fixed = run(&flushed).await;
    assert!(fixed.ok, "{:?}", fixed.issues);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_an_hour_passes_so_a_running_timer_finishes_during_the_check() {
    if !have_chromium() {
        return;
    }
    let timer = variant("(async () => {", "setTimeout(() => finishFocusSession(), 25 * 60 * 1000);\n(async () => {");
    let r = run(&timer).await;
    assert!(has(&r.issues, |i| i.contains("finishFocusSession") && i.contains("for an hour")), "{:?}", r.issues);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_press_that_shows_an_error_is_tried_again_once_the_form_is_complete() {
    if !have_chromium() {
        return;
    }
    // "Save" wants a size picked; the check presses it first (a form's primary action), reads the error, picks a
    // size, and presses again: the core flow runs, and the photo shows the record it made
    let form = head("Coffee", "")
        + r#"<h1>New order</h1><input id="name" aria-label="Order name"><button class="size">Small</button><button class="size">Large</button><p class="err" id="err"></p><ul id="list"></ul><script>const tg = window.Telegram.WebApp; let size = '', state = { v: 1, orders: [] };document.querySelectorAll('.size').forEach((b) => b.onclick = () => { size = b.textContent; });tg.MainButton.setText('Save order').show().onClick(() => {  const name = document.getElementById('name').value.trim();  if (!name || !size) { document.getElementById('err').textContent = 'Pick a size first.'; return; }  document.getElementById('err').textContent = '';  state.orders.push({ name, size }); eve.save(state);  document.getElementById('list').innerHTML = state.orders.map((o) => '<li>' + o.size + '</li>').join(''); });eve.load().then((d) => { if (d) state = d; });</script></body></html>"#;
    let r = run(&form).await;
    assert!(r.ok, "{:?}", r.issues);
    let orders = r.photo_data["orders"].as_array().cloned().unwrap_or_default();
    assert!(!orders.is_empty() && ["Small", "Large"].contains(&orders[0]["size"].as_str().unwrap_or("")), "{}", r.photo_data);
    assert!(r.made().starts_with("orders 1"), "{}", r.made());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_field_is_labelled_by_what_a_person_reads_next_to_it() {
    if !have_chromium() {
        return;
    }
    let labelled = variant(
        r#"<input id="name" placeholder="New habit" aria-label="Habit name" required>"#,
        r#"<label for="name">What to track</label><input id="name" placeholder="e.g. Floss" required>"#,
    );
    let r = run(&labelled).await;
    assert!(r.ok, "{:?}", r.issues);
    assert!(r.trace.iter().any(|t| t == "typing into the field \"What to track\""), "{:?}", r.trace);
    let habits = r.photo_data["habits"].as_array().cloned().unwrap_or_default();
    assert!(habits.iter().any(|h| h["name"] == "Floss"), "{}", r.photo_data);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_toast_over_a_control_is_waited_out_but_a_bar_that_stays_is_reported() {
    if !have_chromium() {
        return;
    }
    let body = r#"<button id="go" style="position:fixed;bottom:8px;left:16px;width:120px">Add item</button><div class="toast">Saved</div>"#;
    let toast = ".toast { position: fixed; bottom: 0; left: 0; width: 200px; height: 70px; background: #333; color: #fff; }";
    let stays = run(&page(body, toast)).await;
    assert!(has(&stays.issues, |i| i.contains("cannot be tapped") && i.contains("\"Saved\"")), "{:?}", stays.issues);
    let goes = run(&page(
        &format!("{body}<script>setTimeout(() => document.querySelector('.toast').remove(), 1500);</script>"),
        toast,
    ))
    .await;
    assert!(goes.ok && goes.taps >= 1, "{:?}", goes.issues);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_the_photo_puts_telegrams_bottom_bar_under_the_page_not_over_it() {
    if !have_chromium() {
        return;
    }
    let html = main_button().replace(
        "</body>",
        r#"<div style="position:fixed;left:0;right:0;bottom:0;height:30px;background:#00ff00"></div></body>"#,
    );
    let shot = run(&html).await.screenshot.expect("a screenshot");
    let img = image::load_from_memory(&shot).unwrap().to_rgb8();
    assert_eq!(img.dimensions(), (780, 1688));
    let p = img.get_pixel(390, (844 - 100 - 15) * 2); // the page's own bottom edge, above the bar
    assert!(p[1] > 200 && p[0] < 80 && p[2] < 80, "{p:?}");
    let p = img.get_pixel(30 * 2, (844 - 100 + 8 + 25) * 2); // the MainButton, beside its label
    assert!(p[2] > 180 && p[0] < 90, "{p:?}");
}

// ---- review fixes: a crash is a finding, nothing leaves by WebSocket, storage throws as when served ----------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_page_the_app_crashes_is_a_finding_and_keeps_what_was_found() {
    if !have_chromium() {
        return;
    }
    let crashing_use: UseFn = Arc::new(|page, sess, _report, _today, _theme| {
        async move {
            sess.note("Console error while tapping \"Add\": boom"); // found before the tab died
            sess.step("tapping \"Stress test\"");
            // what a runaway allocation ends in; the crashed tab never answers
            let _ = tokio::time::timeout(Duration::from_secs(3), page.call("Page.crash", json!({}))).await;
            page.evaluate("1", None).await?;
            Ok(())
        }
        .boxed()
    });
    let r = run_with(&good(), CheckOptions { use_fn: Some(crashing_use), verifier: VerifierChoice::None, ..Default::default() }).await;
    assert!(r.skipped.is_empty() && !r.ok, "{r:?}");
    assert_eq!(r.issues[0], "Console error while tapping \"Add\": boom");
    assert!(has(&r.issues, |i| i.starts_with("The app crashed the page while tapping \"Stress test\"")), "{:?}", r.issues);
    assert!(r.summary().starts_with("2 issue(s)"), "{}", r.summary());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_checker_that_cannot_start_is_still_a_skip() {
    if !have_chromium() {
        return;
    }
    let _permit = GATE.acquire().await.unwrap();
    let b = Arc::new(Browser::launch(&CHROMIUM_ARGS).await.unwrap());
    b.close().await; // "Browser has been closed"
    let r = check_app(&good(), CheckOptions { browser: Some(b), verifier: VerifierChoice::None, ..Default::default() }).await;
    assert!(!r.skipped.is_empty() && r.issues.is_empty(), "{r:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_websocket_never_leaves_the_check_and_is_reported() {
    if !have_chromium() {
        return;
    }
    let srv = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = srv.local_addr().unwrap().port();
    srv.set_nonblocking(true).unwrap();
    let leaky = variant("(async () => {", &format!("new WebSocket('ws://127.0.0.1:{port}/x?d=' + 'secret');\n(async () => {{"));
    let r = run(&leaky).await;
    let hits = std::iter::from_fn(|| srv.accept().ok()).count();
    assert_eq!(hits, 0); // nothing reached the listener
    assert!(has(&r.issues, |i| i.contains("WebSocket") && i.contains("127.0.0.1")), "{:?}", r.issues);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_browser_storage_throws_as_it_does_in_the_served_app() {
    if !have_chromium() {
        return;
    }
    // The server serves apps sandboxed (an opaque origin), so an app leaning on browser storage breaks there.
    let stores = variant("(async () => {", "window['local' + 'Storage'].setItem('x', '1');\n(async () => {");
    let r = run(&stores).await;
    assert!(has(&r.issues, |i| i.contains("sandboxed") && i.contains("localStorage")), "{:?}", r.issues);
}

/// A header like the maker's: the title, and an icon button at its right edge, where eve's Change pencil sits.
fn corner() -> String {
    head("Corner", "header { display: flex; align-items: center; } header h1 { flex: 1; } .icon { width: 44px; height: 44px; }")
        + &GOOD_BODY.replacen("<form", r#"<header><h1>Habits</h1><button class="icon" aria-label="Settings">⚙</button></header><form"#, 1)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_control_under_eves_change_pencil_is_a_finding_and_a_free_corner_is_not() {
    if !have_chromium() {
        return;
    }
    let r = run(&corner()).await;
    let found: Vec<&String> = r.issues.iter().filter(|i| i.contains("Change pencil")).collect();
    assert!(found.len() == 1 && found[0].contains("\"Settings\"") && found[0].contains("padding-right: 56px"), "{:?}", r.issues);
    let free = corner().replace("header { display: flex;", "header { padding-right: 56px; display: flex;");
    let r = run(&free).await;
    assert!(!has(&r.issues, |i| i.contains("Change pencil")), "{:?}", r.issues); // the fix the finding asks for
    assert_eq!(PENCIL, Pencil { top: 6, right: 6, size: 44 });
}

// ---- journeys: Jev walks the maker's journeys on the real page (test_jev_verify.py) -----------------------------

/// A small expense splitter: add a person, add an expense, see who owes whom.
const SPLITTER: &str = r#"<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1"><title>Splitter</title>
<style>body{font:16px system-ui;margin:0;padding:16px} button,input,select{min-height:44px;font-size:16px}</style>
</head><body>
<h1>Splitter</h1>
<label for="who">Name</label><input id="who" placeholder="e.g. Alex">
<button id="addp">Add person</button>
<label for="amt">Amount</label><input id="amt" type="number" placeholder="e.g. 30">
<label for="payer">Paid by</label><select id="payer"></select>
<button id="adde">Add expense</button>
<ul id="people"></ul><p id="bal"></p>
<script>
let s = { v: 1, people: [], exp: [] };
const $ = (i) => document.getElementById(i);
function render() {
  $("people").innerHTML = ""; $("payer").innerHTML = "";
  for (const p of s.people) { const li = document.createElement("li"); li.textContent = p; $("people").appendChild(li);
    const o = document.createElement("option"); o.textContent = p; $("payer").appendChild(o); }
  const total = s.exp.reduce((a, e) => a + e.amt, 0), share = s.people.length ? total / s.people.length : 0;
  $("bal").textContent = s.exp.length ? s.people.map((p) => p + " balance " +
    (s.exp.filter((e) => e.by === p).reduce((a, e) => a + e.amt, 0) - share).toFixed(2)).join(", ") : "No expenses yet";
}
$("addp").onclick = () => { const n = $("who").value.trim(); if (!n) return; s.people.push(n); $("who").value = "";
  render(); eve.save(s); };
$("adde").onclick = () => { /*ADD*/ s.exp.push({ amt: Number($("amt").value), by: $("payer").value }); render(); eve.save(s); };
(async () => { s = (await eve.load()) || s; render(); })();
</script></body></html>"#;

fn broken() -> String {
    SPLITTER.replace("/*ADD*/", "return;") // the negative control: Add expense does nothing
}

fn split_journeys() -> Value {
    json!([{"name": "Split a dinner", "steps": [
        {"do": "type", "target": "Name", "value": "Alex"}, {"do": "tap", "target": "Add person"},
        {"do": "type", "target": "Name", "value": "Sam"}, {"do": "tap", "target": "Add person"},
        {"do": "type", "target": "Amount", "value": "30"}, {"do": "choose", "target": "Paid by", "value": "Sam"},
        {"do": "tap", "target": "Add expense"}],
     "expect": ["Alex balance -15.00, Sam balance 15.00"]}])
}

fn parse(raw: Value) -> Vec<Journey> {
    let (found, problems) = jv::parse_journeys(&raw);
    assert!(problems.is_empty(), "{problems:?}");
    found
}

fn journeys() -> Vec<Journey> {
    parse(split_journeys())
}

/// Answers jev_verify's two questions the way a literal reader would.
fn literal_answer(state: &Value, questions: &Value) -> Value {
    if questions.get("target").is_some() {
        let instructions = questions["present"]["instructions"].as_str().unwrap_or("");
        let label = instructions.split('"').nth(1).unwrap_or("").to_lowercase();
        let hits: Vec<String> = questions["target"]["criteria"]
            .as_object()
            .unwrap()
            .iter()
            .filter(|(k, v)| {
                k.as_str() != jv::STEP_NONE && v.as_str().unwrap_or("").split(" (").next().unwrap_or("").to_lowercase() == label
            })
            .map(|(k, _)| k.clone())
            .collect();
        let key = hits.first().cloned().unwrap_or_else(|| jv::STEP_NONE.to_string());
        return json!({"answers": {"target": {"choice": key, "probabilities": {key.clone(): 0.99}},
                                  "present": {"noul": if hits.is_empty() { 0.03 } else { 0.97 }}},
                      "usage": {"input_tokens": 500}});
    }
    let screen: Vec<&str> = state["screen"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    let screen = screen.join(" ");
    let mut answers = serde_json::Map::new();
    for (q, v) in questions.as_object().unwrap() {
        let text = v["instructions"].as_str().unwrap().split("show: ").nth(1).unwrap().split(" ?").next().unwrap();
        answers.insert(q.clone(), json!({"noul": if screen.contains(text) { 0.96 } else { 0.02 }}));
    }
    json!({"answers": answers, "usage": {"input_tokens": 300}})
}

/// A literal Jev that counts its calls.
fn literal_jev() -> (Predict, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let predict: Predict = Arc::new(move |state: &Value, questions: &Value| {
        c.fetch_add(1, Ordering::SeqCst);
        Ok(literal_answer(state, questions))
    });
    (predict, calls)
}

fn literal() -> VerifierChoice {
    VerifierChoice::Given(Arc::new(Verifier::new(literal_jev().0)))
}

/// Jev answering after `secs`.
fn slow(predict: Predict, secs: f64) -> Predict {
    Arc::new(move |state: &Value, questions: &Value| {
        std::thread::sleep(Duration::from_secs_f64(secs));
        predict(state, questions)
    })
}

async fn walk(html: &str, found: Vec<Journey>, verifier: VerifierChoice) -> CheckReport {
    run_with(html, CheckOptions { journeys: Some(found), verifier, ..Default::default() }).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_working_app_passes_its_journey_and_the_summary_says_so() {
    if !have_chromium() {
        return;
    }
    let (predict, calls) = literal_jev();
    let r = walk(SPLITTER, journeys(), VerifierChoice::Given(Arc::new(Verifier::new(predict)))).await;
    let run = r.journeys.as_ref().expect("journeys");
    assert_eq!(run.passed(), 1, "{}", run.as_dict());
    assert!(r.summary().contains("journeys 1/1"), "{}", r.summary());
    assert!(!has(&r.issues, |i| i.starts_with("Journey")));
    assert_eq!(run.jev["calls"], 8);
    assert_eq!(calls.load(Ordering::SeqCst), 8);
    assert_eq!(run.jev["input_tokens"], 7 * 500 + 300);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_the_broken_add_is_caught_as_an_expectation_not_shown_with_evidence() {
    if !have_chromium() {
        return;
    }
    let r = walk(&broken(), journeys(), literal()).await;
    let run = r.journeys.as_ref().expect("journeys");
    assert!(run.passed() == 0 && !r.ok, "{}", run.as_dict());
    let res = &run.results[0];
    assert!(res.status == "expect" && res.evidence.contains("No expenses yet"), "{}", res.as_dict());
    assert!(r.issues[0].starts_with("Journey \"Split a dinner\"") && r.issues[0].contains("Alex balance -15.00"), "{:?}", r.issues);
    assert!(r.summary().contains("journeys 0/1"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_step_whose_control_is_not_there_fails_at_that_step_and_says_what_is() {
    if !have_chromium() {
        return;
    }
    let mut wrong = split_journeys();
    wrong[0]["steps"] = json!([{"do": "tap", "target": "Add participant"}]);
    let r = walk(SPLITTER, parse(wrong), literal()).await;
    let res = &r.journeys.as_ref().unwrap().results[0];
    assert!(res.status == "step" && res.step == 1, "{}", res.as_dict());
    assert!(res.reason.contains("could not find \"Add participant\"") && res.reason.contains("\"Add person\""), "{}", res.reason);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_jev_down_never_fails_the_app_and_the_journeys_are_not_run() {
    if !have_chromium() {
        return;
    }
    let down: Predict = Arc::new(|_: &Value, _: &Value| Err(anyhow::anyhow!("timed out")));
    let r = walk(&broken(), journeys(), VerifierChoice::Given(Arc::new(Verifier::new(down)))).await;
    let run = r.journeys.as_ref().unwrap();
    assert!(run.not_run.starts_with("Jev did not answer") && run.results.is_empty(), "{}", run.as_dict());
    assert!(!has(&r.issues, |i| i.starts_with("Journey")));
    assert!(r.summary().contains("journeys not run (Jev did not answer"), "{}", r.summary());
    let r2 = walk(SPLITTER, journeys(), VerifierChoice::None).await;
    let run2 = r2.journeys.as_ref().unwrap();
    assert!(run2.not_run == "no verifier" && !has(&r2.issues, |i| i.starts_with("Journey")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_no_journeys_is_the_scripted_check_alone() {
    if !have_chromium() {
        return;
    }
    let r = run_with(SPLITTER, CheckOptions { verifier: literal(), ..Default::default() }).await;
    assert!(r.journeys.is_none() && !r.summary().contains("journeys"), "{}", r.summary());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_journey_step_never_lands_on_a_control_tagged_on_an_earlier_screen() {
    if !have_chromium() {
        return;
    }
    // two views: the first tags "Open" as 0; after it, the form's field must be what id 0 means, not the hidden button
    let html = r#"<!doctype html><html><head><meta name="viewport" content="width=device-width, initial-scale=1">
<title>T</title></head><body><div id="a"><button id="open">Open</button></div>
<div id="b" hidden><label for="n">Name</label><input id="n"><p id="out"></p></div>
<script>
document.getElementById("open").onclick = () => { a.hidden = true; b.hidden = false; };
document.getElementById("n").onchange = (e) => { out.textContent = "Hello " + e.target.value; eve.save({v: 1}); };
eve.load();
</script></body></html>"#;
    let found = parse(json!([{"name": "Greet", "steps": [{"do": "tap", "target": "Open"},
                              {"do": "type", "target": "Name", "value": "Alex"}], "expect": ["Hello Alex"]}]));
    let r = walk(html, found, literal()).await;
    assert_eq!(r.journeys.as_ref().unwrap().passed(), 1, "{}", r.journeys.as_ref().unwrap().as_dict());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_the_main_buttons_text_is_on_the_screen_jev_reads() {
    if !have_chromium() {
        return;
    }
    let html = r#"<!doctype html><html><head><meta name="viewport" content="width=device-width, initial-scale=1">
<title>T</title></head><body><button id="go">Pick</button><script>
const tg = Telegram.WebApp; eve.load();
document.getElementById("go").onclick = () => { tg.MainButton.setText("Clear 1 checked").show(); eve.save({v: 1}); };
tg.MainButton.onClick(() => {});
</script></body></html>"#;
    let found = parse(json!([{"name": "Pick", "steps": [{"do": "tap", "target": "Pick"}], "expect": ["Clear 1 checked"]}]));
    let r = walk(html, found, literal()).await;
    assert_eq!(r.journeys.as_ref().unwrap().passed(), 1, "{}", r.journeys.as_ref().unwrap().as_dict());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_when_jev_goes_down_in_one_journey_the_others_stop_with_the_check() {
    if !have_chromium() {
        return;
    }
    // the other journeys must not go on asking Jev and driving pages after the check returned (the maker's
    // browser stays open, as it does here)
    let asked: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let (lit, _) = literal_jev();
    let log = asked.clone();
    let predict: Predict = Arc::new(move |state: &Value, questions: &Value| {
        let step = state.get("step").map(|s| s.to_string()).unwrap_or_default();
        log.lock().push(step.clone());
        if step.contains("Boom") {
            anyhow::bail!("timed out");
        }
        std::thread::sleep(Duration::from_millis(150));
        lit(state, questions)
    });
    let found = parse(json!([
        {"name": "Slow", "steps": vec![json!({"do": "tap", "target": "Add person"}); 8], "expect": ["No expenses yet"]},
        {"name": "Boom", "steps": [{"do": "tap", "target": "Boom"}], "expect": ["x"]}]));
    let _permit = GATE.acquire().await.unwrap();
    let b = Arc::new(Browser::launch(&CHROMIUM_ARGS).await.unwrap());
    let r = check_app(
        SPLITTER,
        CheckOptions {
            journeys: Some(found),
            verifier: VerifierChoice::Given(Arc::new(Verifier::new(predict))),
            browser: Some(b.clone()),
            ..Default::default()
        },
    )
    .await;
    let at_return = asked.lock().len();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let later = asked.lock().len();
    b.close().await;
    let run = r.journeys.as_ref().unwrap();
    assert!(run.not_run.starts_with("Jev did not answer") && !has(&r.issues, |i| i.starts_with("Journey")), "{}", run.as_dict());
    assert_eq!(later, at_return, "{} more Jev requests after the check returned", later - at_return);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_control_rerendered_while_jev_answers_is_found_again_not_blamed_on_the_app() {
    if !have_chromium() {
        return;
    }
    // a view re-rendered by a timer (a clock, a running timer) replaces the tagged node while Jev answers
    let html = r#"<!doctype html><html><head><meta name="viewport" content="width=device-width, initial-scale=1">
<title>T</title></head><body><div id="view"></div><script>
let n = 0;
function render() { view.innerHTML = '<p>Count ' + n + '</p><p>' + Date.now() + '</p><button id="add">Add one</button>';
  document.getElementById("add").onclick = () => { n++; eve.save({ v: 1, n }); render(); }; }
setInterval(render, 1000); render(); eve.load();
</script></body></html>"#;
    let found = parse(json!([{"name": "Count", "steps": vec![json!({"do": "tap", "target": "Add one"}); 3],
                              "expect": ["Count 3"]}]));
    let r = walk(html, found, VerifierChoice::Given(Arc::new(Verifier::new(slow(literal_jev().0, 0.4))))).await;
    assert_eq!(r.journeys.as_ref().unwrap().passed(), 1, "{}", r.journeys.as_ref().unwrap().as_dict());
}

/// A custom dropdown (a button that opens a list) and a radio group under a legend: both are "choose" steps.
const CHOOSER: &str = r#"<!doctype html><html><head><meta name="viewport" content="width=device-width, initial-scale=1">
<title>T</title><style>button,label{min-height:44px;display:block}</style></head><body>
<button id="cat">Category</button><div id="menu" hidden><button>Food</button><button>Travel</button></div>
<fieldset><legend>Paid by</legend><label><input type="radio" name="p" value="Alex">Alex</label>
<label><input type="radio" name="p" value="Sam">Sam</label></fieldset>
<p id="out">Nothing picked</p><script>
let s = { v: 1, cat: "", by: "" };
const show = () => { out.textContent = "Category: " + (s.cat || "none") + " · Paid by " + (s.by || "nobody"); };
cat.onclick = () => { menu.hidden = !menu.hidden; };
menu.querySelectorAll("button").forEach((b) => b.onclick = () => { s.cat = b.textContent; menu.hidden = true; show(); eve.save(s); });
document.querySelectorAll("input[name=p]").forEach((r) => r.onchange = () => { s.by = r.value; show(); eve.save(s); });
eve.load();
</script></body></html>"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_choose_on_a_custom_dropdown_picks_the_value_not_just_the_opener() {
    if !have_chromium() {
        return;
    }
    let found = parse(json!([{"name": "Pick", "steps": [{"do": "choose", "target": "Category", "value": "Travel"}],
                              "expect": ["Category: Travel"]}]));
    let r = walk(CHOOSER, found, literal()).await;
    assert_eq!(r.journeys.as_ref().unwrap().passed(), 1, "{}", r.journeys.as_ref().unwrap().as_dict());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_choose_in_a_radio_group_named_by_its_legend_picks_the_option() {
    if !have_chromium() {
        return;
    }
    let found = parse(json!([{"name": "Payer", "steps": [{"do": "choose", "target": "Paid by", "value": "Sam"}],
                              "expect": ["Paid by Sam"]}]));
    let r = walk(CHOOSER, found, literal()).await;
    assert_eq!(r.journeys.as_ref().unwrap().passed(), 1, "{}", r.journeys.as_ref().unwrap().as_dict());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_choose_fails_when_the_value_is_nowhere_after_opening_the_target() {
    if !have_chromium() {
        return;
    }
    let found = parse(json!([{"name": "Pick", "steps": [{"do": "choose", "target": "Category", "value": "Rent"}],
                              "expect": ["Category: Rent"]}]));
    let r = walk(CHOOSER, found, literal()).await;
    let res = &r.journeys.as_ref().unwrap().results[0];
    assert!(res.status == "step" && res.reason.contains("could not find \"Rent\""), "{}", res.as_dict());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_journey_that_breaks_the_page_is_its_own_failure_and_the_others_still_count() {
    if !have_chromium() {
        return;
    }
    let breaking: WalkFn = Arc::new(|page, journey, verifier, opts: WalkOptions<'_>| {
        async move {
            if journey.name == "Crash" {
                if let Some(on_step) = opts.on_step {
                    on_step("step 1");
                }
                return Err(jv::PageError::new("Error", "Target crashed").into());
            }
            jv::walk(page, journey, verifier, opts).await
        }
        .boxed()
    });
    let mut found = journeys();
    found.extend(parse(json!([{"name": "Crash", "steps": [{"do": "tap", "target": "Add person"}], "expect": ["x"]}])));
    let r = run_with(
        &broken(),
        CheckOptions { journeys: Some(found), verifier: literal(), walk_fn: Some(breaking), ..Default::default() },
    )
    .await;
    let run = r.journeys.as_ref().unwrap();
    let statuses: Vec<&str> = run.results.iter().map(|x| x.status.as_str()).collect();
    assert!(run.not_run.is_empty() && statuses == ["expect", "step"], "{}", run.as_dict());
    assert!(run.results[1].reason.contains("Target crashed"), "{}", run.results[1].reason);
    assert_eq!(r.issues.iter().filter(|i| i.starts_with("Journey")).count(), 2);
    assert!(r.summary().contains("journeys 0/2"), "{}", r.summary());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_jev_down_in_one_journey_keeps_the_journeys_that_already_failed() {
    if !have_chromium() {
        return;
    }
    let (lit, _) = literal_jev();
    let predict: Predict = Arc::new(move |state: &Value, questions: &Value| {
        if state.get("step").map(|s| s.to_string()).unwrap_or_default().contains("Boom") {
            std::thread::sleep(Duration::from_secs(2)); // the split journey finishes (and fails) before Jev goes down
            anyhow::bail!("timed out");
        }
        lit(state, questions)
    });
    let mut found = journeys();
    found.extend(parse(json!([{"name": "Boom", "steps": [{"do": "tap", "target": "Boom"}], "expect": ["x"]}])));
    let r = walk(&broken(), found, VerifierChoice::Given(Arc::new(Verifier::new(predict)))).await;
    let run = r.journeys.as_ref().unwrap();
    let names: Vec<&str> = run.results.iter().map(|x| x.name.as_str()).collect();
    assert!(names == ["Split a dinner"] && run.results[0].status == "expect", "{}", run.as_dict());
    assert!(run.not_run.starts_with("Jev did not answer"));
    assert!(has(&r.issues, |i| i.starts_with("Journey \"Split a dinner\"")) && !r.ok);
    assert!(r.summary().contains("journeys 0/2 (1 not run: Jev did not answer"), "{}", r.summary());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_one_failed_jev_request_is_asked_again_before_giving_up() {
    if !have_chromium() {
        return;
    }
    let (lit, _) = literal_jev();
    let failed = Arc::new(AtomicUsize::new(0));
    let flaky: Predict = Arc::new(move |state: &Value, questions: &Value| {
        if failed.swap(1, Ordering::SeqCst) == 0 {
            anyhow::bail!("read timed out");
        }
        lit(state, questions)
    });
    let r = walk(SPLITTER, journeys(), VerifierChoice::Given(Arc::new(Verifier::new(flaky)))).await;
    let run = r.journeys.as_ref().unwrap();
    assert!(run.passed() == 1 && run.not_run.is_empty(), "{}", run.as_dict());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_an_app_that_hangs_in_a_journey_fails_that_journey_in_seconds() {
    if !have_chromium() {
        return;
    }
    let html = r#"<!doctype html><html><head><meta name="viewport" content="width=device-width, initial-scale=1">
<title>T</title></head><body><label for="n">Name</label><input id="n"><p>Hi</p><script>
n.onchange = (e) => { if (e.target.value === "spin-forever") for (;;) {} eve.save({ v: 1 }); }; eve.load();
</script></body></html>"#;
    let found = parse(json!([{"name": "Hang", "steps": [{"do": "type", "target": "Name", "value": "spin-forever"},
                                                         {"do": "type", "target": "Name", "value": "b"}],
                              "expect": ["Hi b"]}]));
    let _permit = GATE.acquire().await.unwrap();
    let t0 = Instant::now();
    let r = check_app(html, CheckOptions { journeys: Some(found), verifier: literal(), eval_timeout_s: 2.0, ..Default::default() }).await;
    assert!(t0.elapsed().as_secs_f64() < 30.0);
    let res = &r.journeys.as_ref().unwrap().results[0];
    assert!(res.status == "step" && res.reason.contains("stopped responding"), "{}", res.as_dict());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_journeys_are_skipped_when_the_scripted_pass_already_found_the_app_hung() {
    if !have_chromium() {
        return;
    }
    let html = r#"<!doctype html><html><head><meta name="viewport" content="width=device-width, initial-scale=1">
<title>T</title></head><body><button id="go">Start</button><script>
go.onclick = () => { for (;;) {} }; eve.load();
</script></body></html>"#;
    let found = parse(json!([{"name": "Go", "steps": [{"do": "tap", "target": "Start"}], "expect": ["x"]}]));
    let (predict, calls) = literal_jev();
    let _permit = GATE.acquire().await.unwrap();
    let t0 = Instant::now();
    let r = check_app(
        html,
        CheckOptions {
            journeys: Some(found),
            verifier: VerifierChoice::Given(Arc::new(Verifier::new(predict))),
            check_timeout_s: 6.0,
            ..Default::default()
        },
    )
    .await;
    assert!(t0.elapsed().as_secs_f64() < 25.0);
    assert!(has(&r.issues, |i| i.contains("stopped responding")), "{:?}", r.issues);
    assert!(r.journeys.as_ref().unwrap().not_run.starts_with("the app stopped responding"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_the_owners_photo_is_the_last_screen_of_a_journey_that_passed() {
    if !have_chromium() {
        return;
    }
    let r = walk(SPLITTER, journeys(), literal()).await;
    assert_eq!(r.photo_from, "the journey \"Split a dinner\"");
    assert_eq!(&r.screenshot.as_ref().unwrap()[..2], b"\xff\xd8");
    assert_eq!(r.photo_data["people"], json!(["Alex", "Sam"])); // the journey's data, not the script's
    let broken = walk(&broken(), journeys(), literal()).await;
    assert_eq!(broken.photo_from, "the scripted check"); // no journey passed: the script's picture
}

// ---- the page driver itself --------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_page_driver_acts_as_playwrights_locators_do() {
    if !have_chromium() {
        return;
    }
    let _permit = GATE.acquire().await.unwrap();
    let b = Browser::launch(&CHROMIUM_ARGS).await.unwrap();
    let ctx = b
        .new_context(ContextOptions {
            width: 390,
            height: 844,
            device_scale_factor: 2.0,
            is_mobile: true,
            has_touch: true,
            locale: "en-US".into(),
            color_scheme: "light".into(),
        })
        .await
        .unwrap();
    let page = ctx.new_page().await.unwrap();
    page.set_content(
        r#"<input id="t"><input id="d" type="date"><input id="ro" readonly><select id="s"><option>a</option><option>b</option></select>
<button id="b" onclick="this.textContent='hit'">tap</button><button id="off" disabled>off</button>
<div id="cover" style="position:fixed;inset:0;display:none"></div><p id="log"></p>
<script>document.getElementById('t').addEventListener('change', (e) => log.textContent = 'changed ' + e.target.value);</script>"#,
    )
    .await
    .unwrap();
    page.fill("#t", "hello", 1000).await.unwrap();
    page.evaluate_on("#t", "(el) => el.blur()", 1000).await.unwrap();
    assert_eq!(page.evaluate("() => log.textContent", None).await.unwrap(), "changed hello");
    page.fill("#t", "again", 1000).await.unwrap(); // typed over, not appended
    assert_eq!(page.evaluate("() => t.value", None).await.unwrap(), "again");
    page.fill("#d", "2026-09-24", 1000).await.unwrap();
    assert_eq!(page.evaluate("() => d.value", None).await.unwrap(), "2026-09-24");
    let ro = page.fill("#ro", "x", 300).await.unwrap_err();
    assert_eq!(ro.downcast_ref::<jv::PageError>().unwrap().kind, "TimeoutError");
    assert_eq!(ro.to_string(), "Locator.fill: Timeout 300ms exceeded.");
    page.select_index("#s", 1, 1000).await.unwrap();
    assert_eq!(page.evaluate("() => s.value", None).await.unwrap(), "b");
    page.select_label("#s", Some("a"), 1000).await.unwrap();
    assert_eq!(page.evaluate("() => s.value", None).await.unwrap(), "a");
    page.click("#b", 1000).await.unwrap();
    assert_eq!(page.evaluate("() => b.textContent", None).await.unwrap(), "hit");
    assert!(page.click("#off", 300).await.is_err()); // disabled: waited for, never clicked
    page.evaluate("() => { cover.style.display = 'block'; }", None).await.unwrap();
    assert!(page.click("#b", 300).await.is_err()); // covered
    assert_eq!(page.count("button").await.unwrap(), 2);
    assert!(page.click("#nothing", 200).await.is_err());
    let thrown = page.evaluate("() => { throw new TypeError('nope'); }", None).await.unwrap_err().to_string();
    assert!(thrown.starts_with("Page.evaluate: TypeError: nope"), "{thrown}");
    assert_eq!(page.evaluate("1 + 1", None).await.unwrap(), 2);
    assert_eq!(page.evaluate("async (x) => x * 3", Some(json!(4))).await.unwrap(), 12);
    ctx.close().await;
    b.close().await;
}
