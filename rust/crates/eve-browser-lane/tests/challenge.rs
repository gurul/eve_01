//! Port of test_browser_challenge.py and test_challenge_settle.py: a bot-check page ends the browser lane as
//! "blocked", with a line for the owner, and nothing is clicked; a Cloudflare "Just a moment…" page is read
//! again for a few seconds first, hard blocks stop at once. The clock and the sleep are injected: no test sleeps.

mod common;

use std::sync::Arc;

use common::*;
use eve_browser_lane as bl;
use parking_lot::Mutex;
use serde_json::{json, Value};

const CF_TEXT: &str = "Checking your browser before accessing example.com. This process is automatic.";

#[test]
fn test_challenges_are_recognised() {
    let cases: &[(&str, &str, &str)] = &[
        ("https://www.google.com/sorry/index?continue=https://www.google.com/search%3Fq%3Dramen", "", "Our systems have detected unusual traffic"),
        ("https://ipv4.google.com/sorry/index?q=x", "", ""),
        ("https://www.ebay.com/splashui/challenge?ap=1&appName=orch", "Security Measure", ""),
        ("https://shop.example/", "Just a moment...", "Enable JavaScript and cookies to continue"),
        ("https://shop.example/", "Just a moment…", ""),
        ("https://shop.example/", "", CF_TEXT),
        ("https://shop.example/", "", "Verifying you are human. shop.example needs to review the security of your connection before proceeding."),
        ("https://www.amazon.com/errors/validateCaptcha", "Robot Check", "Enter the characters you see below"),
        ("https://shop.example/item", "Access Denied", "You don't have permission to access this server."),
        ("https://www.zillow.com/homes/", "Pardon Our Interruption", "As you were browsing something about your browser made us think you were a bot."),
        ("https://shop.example/", "Attention Required! | Cloudflare", "Sorry, you have been blocked"),
    ];
    for (url, title, text) in cases {
        assert_eq!(bl::browser_blocker(url, title, text), "browser_challenge", "{url} {title}");
    }
}

#[test]
fn test_ordinary_pages_are_never_challenges() {
    let long = format!("{} Checking your browser before accessing", "x".repeat(600));
    let cases: Vec<(&str, &str, &str)> = vec![
        ("https://www.google.com/search?q=sorry", "sorry - Google Search", "About 1,000,000 results"),
        ("https://www.google.com/", "Google", "Google Search I'm Feeling Lucky"),
        ("https://shop.example/sorry/", "Sorry, we're closed today", "Come back tomorrow."),
        ("https://docs.aws.amazon.com/s3/troubleshoot", "Troubleshoot Access Denied (403 Forbidden) errors in Amazon S3",
         "If you get an access denied error, check the bucket policy."),
        ("https://en.wikipedia.org/wiki/Robot", "Robot - Wikipedia", "A robot is a machine capable of carrying out actions"),
        ("https://news.ycombinator.com/", "Hacker News", "1. Show HN: robot check for my homelab"),
        ("https://blog.cloudflare.com/x", "How our challenge pages work", &long),
        ("https://shop.example/", "Just a moment of calm: a meditation guide", "Breathe in."),
        ("https://shop.example/cart", "Your cart", "Access denied? Sign in again to see your saved items."),
        ("about:blank", "", ""),
        ("", "", ""),
    ];
    for (url, title, text) in cases {
        assert_eq!(bl::browser_blocker(url, title, text), "", "{url} {title}");
    }
}

#[test]
fn test_the_owner_line_names_the_site_and_says_what_to_do() {
    let line = bl::blocked_line("https://www.google.com/sorry/index?continue=secret-query");
    assert!(line.contains("google.com") && !line.contains("secret-query"));
    assert!(line.contains("bot check") && line.contains("ask me again"));
    assert!(bl::blocked_line("").contains("that site"));
}

fn challenge_page() -> FakePage {
    FakePage::new(
        "https://www.google.com/sorry/index?continue=x",
        "",
        "Our systems have detected unusual traffic from your computer network.",
        vec![
            row(json!({"id": "0", "role": "checkbox", "name": "I'm not a robot", "value": "unchecked", "x": 10, "y": 10,
                       "w": 20, "h": 20, "enabled": true, "href": ""})),
            button("1", "Submit"),
        ],
    )
}

fn ordinary_page() -> FakePage {
    FakePage::new(
        "https://shop.example/",
        "Eve Shop",
        "Welcome to the shop. Access denied pages are rare here.",
        vec![link("0", "Top stories", "https://shop.example/news"), button("1", "Search")],
    )
}

#[test]
fn test_snapshot_marks_a_challenge_and_leaves_an_ordinary_page_alone() {
    let p = bl::Page::new(challenge_page().boxed());
    p.snapshot().unwrap();
    assert!(!p.blocked().is_empty() && p.blocked().contains("google.com"));
    let q = bl::Page::new(ordinary_page().boxed());
    q.snapshot().unwrap();
    assert_eq!(q.blocked(), "");
}

#[test]
fn test_effectors_touch_nothing_on_a_challenge_page() {
    let page = challenge_page();
    let p = bl::Page::new(page.boxed());
    let snap = p.snapshot().unwrap();
    let robot = snap.elements.iter().find(|c| c.label == "I'm not a robot").unwrap();
    assert!(p.click_candidate(robot).starts_with("refused: blocked:"));
    assert!(p.focus_and_type(robot, "x").starts_with("refused: blocked:"));
    assert!(p.press("return").unwrap().starts_with("refused: blocked:"));
    assert!(page.clicks().is_empty() && page.keys().is_empty());
}

fn click_robot() -> Value {
    json!({"steps": [{"kind": "click", "target": "the I'm not a robot checkbox", "label_hint": "I'm not a robot"}],
           "final_say": "Done.", "success": {"kind": "text_visible", "value": "results"}})
}

#[test]
fn test_a_plan_on_a_challenge_page_ends_blocked_without_a_click() {
    let tmp = tempfile::tempdir().unwrap();
    let page = challenge_page();
    let lane = lane_on(&page, tmp.path());
    let result = run(closing(&lane, lane.run_plan(click_robot(), "search google for ramen", 0, no_approvals(), false)))
        .unwrap();
    assert_eq!(result["status"], "blocked", "{result}");
    assert_eq!(result["reason"], "browser_challenge");
    let sentence = result["sentence"].as_str().unwrap();
    assert!(sentence.contains("google.com") && sentence.contains("bot check"));
    assert!(page.clicks().is_empty() && page.keys().is_empty());
}

#[test]
fn test_a_plan_that_lands_on_a_challenge_mid_run_ends_blocked() {
    let tmp = tempfile::tempdir().unwrap();
    let sorry = "https://www.google.com/sorry/index?continue=q";
    let page = FakePage::new("https://shop.example/", "Eve Shop", "Welcome", vec![button("0", "Search")]).with_route(
        sorry,
        "",
        "Our systems have detected unusual traffic",
        vec![button("0", "Submit")],
    );
    let plan = json!({"steps": [{"kind": "open_url", "target": sorry},
                                {"kind": "click", "target": "the Submit button", "label_hint": "Submit"}],
                      "final_say": "Done.", "success": {"kind": "text_visible", "value": "results"}});
    let mut core = core_on(&page, tmp.path());
    core.plain_http.insert(sorry.into(), sorry.into());
    let lane = bl::BrowserLane::from_core(core).unwrap();
    let result = run(closing(&lane, lane.run_plan(plan, "search google for ramen", 0, no_approvals(), false))).unwrap();
    assert_eq!(result["status"], "blocked", "{result}");
    assert!(page.clicks().is_empty());
}

#[test]
fn test_an_ordinary_plan_is_not_blocked() {
    let tmp = tempfile::tempdir().unwrap();
    let page = ordinary_page();
    let lane = lane_on(&page, tmp.path());
    let plan = json!({"steps": [{"kind": "click", "target": "the Search button", "label_hint": "Search"}],
                      "final_say": "Searched.", "success": {"kind": "text_visible", "value": "Welcome"}});
    let result =
        run(closing(&lane, lane.run_plan(plan, "click search on the shop site", 0, no_approvals(), false))).unwrap();
    assert_ne!(result["status"], "blocked", "{result}");
    assert_eq!(page.clicks(), vec!["1"]);
}

#[test]
fn test_outline_and_page_text_carry_the_owner_line() {
    let tmp = tempfile::tempdir().unwrap();
    let lane = lane_on(&challenge_page(), tmp.path());
    let (outline, page) = run(closing(&lane, async {
        (lane.outline().await.unwrap(), lane.page_text(bl::PAGE_TEXT_LIMIT).await.unwrap())
    }));
    assert!(outline.blocked.contains("google.com"));
    assert_eq!(page.blocked, outline.blocked);
    let lane2 = lane_on(&ordinary_page(), tmp.path());
    let outline2 = run(closing(&lane2, lane2.outline())).unwrap();
    assert_eq!(outline2.blocked, "");
}

// ---- test_challenge_settle.py -------------------------------------------------------------------------------

const CF_SHOP: &str = "Checking your browser before accessing shop.example. This process is automatic.";

/// A fake clock and sleep. `clear_at`: when the fake page swaps the check for the real page.
#[derive(Clone)]
struct Clock {
    t: Arc<Mutex<f64>>,
    slept: Arc<Mutex<Vec<f64>>>,
    page: FakePage,
    clear_at: Option<f64>,
}

impl Clock {
    fn new(page: &FakePage, clear_at: Option<f64>) -> Clock {
        Clock { t: Arc::new(Mutex::new(100.0)), slept: Arc::default(), page: page.clone(), clear_at }
    }

    fn clock(&self) -> bl::ClockFn {
        let t = self.t.clone();
        Arc::new(move || *t.lock())
    }

    fn sleep(&self) -> bl::SleepFn {
        let me = self.clone();
        Arc::new(move |secs| {
            me.slept.lock().push(secs);
            let now = {
                let mut t = me.t.lock();
                *t += secs;
                *t
            };
            let mut st = me.page.state();
            if me.clear_at.is_some_and(|at| now - 100.0 >= at) && st.title.starts_with("Just") {
                st.url = "https://shop.example/item".into();
                st.title = "Blue mug".into();
                st.body = "Blue mug, 12 dollars. Add to cart.".into();
                st.elements = vec![button("0", "Add to cart"), link("1", "Reviews", "https://shop.example/r")];
                st.doc += 1.0; // Cloudflare loads the real page as a new document
            }
        })
    }

    fn options(&self) -> bl::PageOptions {
        bl::PageOptions { clock: Some(self.clock()), sleep: Some(self.sleep()) }
    }

    fn slept(&self) -> Vec<f64> {
        self.slept.lock().clone()
    }

    fn total(&self) -> f64 {
        self.slept().iter().sum()
    }
}

fn cloudflare_page() -> FakePage {
    FakePage::new(
        "https://shop.example/item",
        "Just a moment...",
        "Enable JavaScript and cookies to continue",
        vec![link("0", "Cloudflare", "https://www.cloudflare.com/")],
    )
}

#[test]
fn test_cloudflare_checks_are_soft() {
    for (url, title, text) in [
        ("https://shop.example/", "Just a moment...", "Enable JavaScript and cookies to continue"),
        ("https://shop.example/", "Just a moment…", ""),
        ("https://shop.example/", "", CF_SHOP),
        ("https://shop.example/", "", "Verifying you are human. shop.example needs to review the security of your connection before proceeding."),
    ] {
        assert_eq!(bl::browser_blocker(url, title, text), "browser_challenge");
        assert_eq!(bl::challenge_kind(url, title, text), "soft");
    }
}

#[test]
fn test_hard_blocks_are_hard() {
    for (url, title, text) in [
        ("https://www.google.com/sorry/index?continue=x", "", "Our systems have detected unusual traffic"),
        ("https://www.ebay.com/splashui/challenge?ap=1", "Security Measure", ""),
        ("https://shop.example/", "Attention Required! | Cloudflare", "Sorry, you have been blocked"),
        ("https://shop.example/item", "Access Denied", "You don't have permission to access this server."),
        ("https://www.amazon.com/errors/validateCaptcha", "Robot Check", ""),
        ("https://www.zillow.com/homes/", "Pardon Our Interruption", ""),
    ] {
        assert_eq!(bl::challenge_kind(url, title, text), "hard", "{url}");
    }
}

#[test]
fn test_ordinary_pages_have_no_kind() {
    assert_eq!(bl::challenge_kind("https://shop.example/", "Just a moment of calm", "Breathe in."), "");
    assert_eq!(bl::challenge_kind("", "", ""), "");
}

#[test]
fn test_a_check_that_clears_in_five_seconds_is_waited_out() {
    let page = cloudflare_page();
    let clock = Clock::new(&page, Some(5.0));
    let p = bl::Page::new(page.boxed()).with_options(&clock.options());
    let snap = p.snapshot().unwrap();
    assert_eq!(p.blocked(), "");
    let labels: Vec<&str> = snap.elements.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, ["Add to cart", "Reviews"]); // the real page, not the check
    assert!((5.0..=bl::CHALLENGE_SETTLE_SECS).contains(&clock.total()));
    assert_eq!(snap.title, "Blue mug");
    // The recorded document is the real page's, so a click on it is not refused as stale.
    let reviews = snap.elements.iter().find(|c| c.label == "Reviews").unwrap();
    let said = p.click_candidate(reviews);
    assert!(said.starts_with("clicked"), "{said}");
    assert_eq!(page.clicks(), vec!["1"]);
}

#[test]
fn test_a_check_that_never_clears_blocks_after_a_bounded_wait() {
    let page = cloudflare_page();
    let clock = Clock::new(&page, None);
    let p = bl::Page::new(page.boxed()).with_options(&clock.options());
    p.snapshot().unwrap();
    assert!(!p.blocked().is_empty() && p.blocked().contains("shop.example"));
    let total = clock.total();
    assert!((bl::CHALLENGE_SETTLE_SECS - bl::CHALLENGE_POLL_SECS..=bl::CHALLENGE_SETTLE_SECS).contains(&total));
    assert!((7.0..=10.0).contains(&bl::CHALLENGE_SETTLE_SECS));
    // A second look at the same stuck check does not wait again: the plan is already stopping.
    let before = clock.slept().len();
    p.snapshot().unwrap();
    assert!(!p.blocked().is_empty() && clock.slept().len() == before);
}

#[test]
fn test_the_wait_is_bounded_even_when_the_clock_does_not_move() {
    let page = cloudflare_page();
    let sleeps: Arc<Mutex<Vec<f64>>> = Arc::default();
    let s = sleeps.clone();
    let p = bl::Page::new(page.boxed()).with_clock(Arc::new(|| 5.0)).with_sleep(Arc::new(move |x| s.lock().push(x)));
    p.snapshot().unwrap();
    assert!(!p.blocked().is_empty());
    assert!(sleeps.lock().len() <= (bl::CHALLENGE_SETTLE_SECS / bl::CHALLENGE_POLL_SECS) as usize + 1);
}

#[test]
fn test_hard_blocks_stop_at_once() {
    for (url, title, body) in [
        ("https://www.google.com/sorry/index?continue=x", "", "Our systems have detected unusual traffic"),
        ("https://shop.example/", "Attention Required! | Cloudflare", "Sorry, you have been blocked"),
        ("https://shop.example/item", "Access Denied", "You don't have permission to access this server."),
        ("https://www.ebay.com/splashui/challenge?ap=1", "Security Measure", "Please verify yourself"),
    ] {
        let page = FakePage::new(url, title, body, vec![button("0", "Submit")]);
        let clock = Clock::new(&page, None);
        let p = bl::Page::new(page.boxed()).with_options(&clock.options());
        p.snapshot().unwrap();
        assert!(!p.blocked().is_empty());
        assert!(clock.slept().is_empty());
    }
}

#[test]
fn test_an_ordinary_page_never_waits() {
    let page = FakePage::new("https://shop.example/", "Eve Shop", "Welcome", vec![button("0", "Search")]);
    let clock = Clock::new(&page, None);
    let p = bl::Page::new(page.boxed()).with_options(&clock.options());
    p.snapshot().unwrap();
    assert!(p.blocked().is_empty() && clock.slept().is_empty());
}

#[test]
fn test_a_plan_on_a_check_that_clears_runs_on_the_real_page() {
    let tmp = tempfile::tempdir().unwrap();
    let page = cloudflare_page();
    let clock = Clock::new(&page, Some(5.0));
    let mut core = core_on(&page, tmp.path());
    core.page_options = clock.options();
    let lane = bl::BrowserLane::from_core(core).unwrap();
    let plan = json!({"steps": [{"kind": "click", "target": "the Reviews link", "label_hint": "Reviews"}],
                      "final_say": "Opened.", "success": {"kind": "text_visible", "value": "Blue mug"}});
    let result = run(closing(
        &lane,
        lane.run_plan(plan, "open the blue mug reviews on shop.example", 0, no_approvals(), false),
    ))
    .unwrap();
    assert_ne!(result["status"], "blocked", "{result}");
    assert_eq!(page.clicks(), vec!["1"]);
}

#[test]
fn test_page_text_waits_out_a_check_that_clears() {
    let tmp = tempfile::tempdir().unwrap();
    let page = cloudflare_page();
    let clock = Clock::new(&page, Some(5.0));
    let mut core = core_on(&page, tmp.path());
    core.page_options = clock.options();
    let lane = bl::BrowserLane::from_core(core).unwrap();
    let read = run(closing(&lane, lane.page_text(bl::PAGE_TEXT_LIMIT))).unwrap();
    assert_eq!(read.blocked, "", "{read:?}");
    assert!(read.text.contains("Blue mug"));
    assert!(clock.total() <= bl::CHALLENGE_SETTLE_SECS + bl::READ_WAIT_SECS);
}

#[test]
fn test_page_text_blocks_a_hard_block_without_waiting() {
    let tmp = tempfile::tempdir().unwrap();
    let page =
        FakePage::new("https://www.google.com/sorry/index?continue=x", "", "Our systems have detected unusual traffic", vec![]);
    let clock = Clock::new(&page, None);
    let mut core = core_on(&page, tmp.path());
    core.page_options = clock.options();
    let lane = bl::BrowserLane::from_core(core).unwrap();
    let read = run(closing(&lane, lane.page_text(bl::PAGE_TEXT_LIMIT))).unwrap();
    assert!(read.blocked.contains("google.com"));
    assert!(clock.slept().is_empty());
}
