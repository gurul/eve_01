//! Port of test_browser_attach.py and the lane half of test_steel_local.py / test_chrome_lane.py: attach mode
//! drives the owner's running Chrome through DevToolsActivePort, in its own tab only; eve's own Chrome and
//! Steel never ask; which Chrome profile a request names.

mod common;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use eve_browser_lane as bl;
use eve_browser_lane::driver::{same_context, ContextHandle, Driver};
use eve_desktop_lanes::error_message;
use indexmap::IndexMap;
use parking_lot::Mutex;

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn attach_cfg() -> bl::BrowserLaneConfig {
    bl::BrowserLaneConfig { attach: true, ..Default::default() }
}

fn is_attach_error(e: &anyhow::Error, needle: &str) -> bool {
    e.downcast_ref::<bl::AttachError>().is_some_and(|a| a.0.contains(needle))
}

#[test]
fn test_endpoint_is_read_like_chrome_devtools_mcp() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("DevToolsActivePort"), "64687\n/devtools/browser/600e426b-7753\n").unwrap();
    assert_eq!(
        bl::devtools_endpoint(tmp.path(), bl::DEFAULT_DEBUG_PORT).as_deref(),
        Some("ws://127.0.0.1:64687/devtools/browser/600e426b-7753")
    );
}

#[test]
fn test_a_missing_or_malformed_file_means_no_endpoint() {
    for content in ["", "64687\n", "abc\n/devtools/browser/x\n", "70000\n/devtools/browser/x\n", "64687\n/json/version\n"] {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(bl::devtools_endpoint(tmp.path(), bl::DEFAULT_DEBUG_PORT), None); // no file at all
        std::fs::write(tmp.path().join("DevToolsActivePort"), content).unwrap();
        assert_eq!(bl::devtools_endpoint(tmp.path(), bl::DEFAULT_DEBUG_PORT), None, "{content:?}");
    }
}

#[test]
fn test_attach_is_off_unless_asked() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(!bl::configured(Some(&env(&[]))).attach);
    let dir = tmp.path().to_str().unwrap();
    let cfg = bl::configured(Some(&env(&[("EVE_BROWSER_ATTACH", "1"), ("EVE_CHROME_DIR", dir)])));
    assert!(cfg.attach && cfg.chrome_dir == tmp.path());
}

#[test]
fn test_attach_without_remote_debugging_says_how_to_switch_it_on() {
    let tmp = tempfile::tempdir().unwrap();
    let mut core = core_with(bl::BrowserLaneConfig { attach: true, chrome_dir: tmp.path().into(), ..Default::default() });
    let err = core.ensure().err().unwrap();
    assert!(is_attach_error(&err, "chrome://inspect/#remote-debugging"), "{err}");
}

/// A core attached to fakes that log what is closed: eve's tab, the context, the driver.
fn attached_with_log(cfg: bl::BrowserLaneConfig, calls: &Arc<Mutex<Vec<String>>>) -> bl::LaneCore {
    let mut core = core_with(cfg);
    let tab = FakePage::new("https://x/", "eve", "", vec![]);
    tab.state().close_log = Some(calls.clone());
    let mut ctx = FakeContext::new(tab.clone());
    ctx.log = Some(calls.clone());
    let ctx: Arc<dyn ContextHandle> = Arc::new(ctx);
    core.browser = Some(Arc::new(FakeBrowser { contexts: vec![ctx.clone()], connected: true }));
    core.context = Some(ctx);
    core.pw = Some(Box::new(FakeDriver::logging(calls.clone())));
    core.page = Some(bl::Page::new(tab.boxed()));
    core
}

#[test]
fn test_closing_in_attach_mode_closes_only_eves_tab_never_the_owners_browser() {
    let calls: Arc<Mutex<Vec<String>>> = Arc::default();
    let mut core = attached_with_log(attach_cfg(), &calls);
    core.close();
    assert_eq!(*calls.lock(), ["eve tab.close", "playwright.stop"]); // never the context, never the browser
    assert!(core.browser.is_none() && core.context.is_none() && core.pw.is_none() && core.page.is_none());
}

#[test]
fn test_enter_is_a_key() {
    assert_eq!(bl::key_name("enter"), Some("Enter"));
    assert_eq!(bl::key_name("return"), Some("Enter"));
}

#[test]
fn test_a_protected_file_falls_back_to_the_port_and_chromes_own_prompt() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("DevToolsActivePort");
    std::fs::write(&file, "9222\n/devtools/browser/x\n").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&file).is_ok() {
        eprintln!("skipped: this user can read a mode-000 file (root)");
        return;
    }
    let listening = |port: i64| port == 9333;
    assert_eq!(
        bl::devtools_endpoint_with(tmp.path(), 9333, &listening).as_deref(),
        Some("ws://127.0.0.1:9333/devtools/browser")
    );
    assert_eq!(bl::devtools_endpoint_with(tmp.path(), 9222, &listening), None); // nothing listening: off
}

#[test]
fn test_the_debug_port_is_configurable() {
    assert_eq!(bl::configured(Some(&env(&[]))).debug_port, 9222);
    assert_eq!(bl::configured(Some(&env(&[("EVE_CHROME_DEBUG_PORT", "9333")]))).debug_port, 9333);
    assert_eq!(bl::configured(Some(&env(&[("EVE_CHROME_DEBUG_PORT", "nope")]))).debug_port, 9222);
}

/// A web app that says Loading… first, then fills in.
#[derive(Clone)]
struct GrowingPage {
    texts: Arc<Mutex<Vec<String>>>,
    url: String,
}

impl GrowingPage {
    fn new(texts: &[&str], url: &str) -> GrowingPage {
        GrowingPage { texts: Arc::new(Mutex::new(texts.iter().map(|s| s.to_string()).collect())), url: url.into() }
    }
}

impl eve_browser_lane::driver::PageHandle for GrowingPage {
    fn evaluate(&self, _s: &str, _a: Option<&serde_json::Value>) -> anyhow::Result<serde_json::Value> {
        Ok(serde_json::Value::Null)
    }
    fn title(&self) -> anyhow::Result<String> {
        Ok("Inbox".into())
    }
    fn url(&self) -> String {
        self.url.clone()
    }
    fn inner_text(&self, _s: &str, _t: u64) -> anyhow::Result<String> {
        let mut texts = self.texts.lock();
        Ok(if texts.len() > 1 { texts.remove(0) } else { texts[0].clone() })
    }
    fn count(&self, _s: &str) -> anyhow::Result<usize> {
        Ok(0)
    }
    fn scroll_into_view_if_needed(&self, _s: &str, _t: u64) -> anyhow::Result<()> {
        Ok(())
    }
    fn click(&self, _s: &str, _t: u64) -> anyhow::Result<()> {
        Ok(())
    }
    fn fill(&self, _s: &str, _v: &str, _t: u64) -> anyhow::Result<()> {
        Ok(())
    }
    fn select_option(&self, _s: &str, _c: bl::driver::SelectBy<'_>, _t: u64) -> anyhow::Result<()> {
        Ok(())
    }
    fn evaluate_on(&self, _s: &str, _script: &str) -> anyhow::Result<serde_json::Value> {
        Ok(serde_json::Value::Null)
    }
    fn keyboard_type(&self, _t: &str, _d: u64) -> anyhow::Result<()> {
        Ok(())
    }
    fn keyboard_press(&self, _k: &str) -> anyhow::Result<()> {
        Ok(())
    }
    fn wait_for_timeout(&self, _ms: u64) {}
    fn wait_for_load_state(&self, _s: &str, _t: u64) -> anyhow::Result<()> {
        Ok(())
    }
    fn goto(&self, _u: &str, _w: &str, _t: u64) -> anyhow::Result<()> {
        Ok(())
    }
    fn bring_to_front(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn close(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn screenshot_jpeg(&self, _q: u8, _m: Option<(&str, &str)>) -> anyhow::Result<Vec<u8>> {
        Ok(vec![])
    }
}

/// `_lane_on`: a lane whose page is `page`, with READ_POLL_SECS' wait made instant.
fn growing_core(page: GrowingPage) -> bl::LaneCore {
    let mut core = core_with(bl::BrowserLaneConfig::default());
    core.page = Some(bl::Page::new(Box::new(page)).with_sleep(Arc::new(|_| {})));
    core
}

#[test]
fn test_page_text_waits_for_a_loading_app_to_fill_in() {
    let full = format!("Inbox {}", "message ".repeat(80));
    let mut core = growing_core(GrowingPage::new(
        &["Loading…", "Loading… Gmail", &full, &full],
        "https://mail.google.com/mail/u/0/#inbox",
    ));
    let got = core.page_text(bl::PAGE_TEXT_LIMIT).unwrap();
    assert_eq!(got.text, full.split_whitespace().collect::<Vec<_>>().join(" "));
    assert!(!got.signed_out);
}

#[test]
fn test_page_text_notices_a_sign_in_redirect() {
    let text = "Sign in to GitHub ".repeat(30);
    let mut core = growing_core(GrowingPage::new(
        &[&text],
        "https://github.com/login?return_to=https%3A%2F%2Fgithub.com%2Fnotifications",
    ));
    assert!(core.page_text(bl::PAGE_TEXT_LIMIT).unwrap().signed_out);
    for url in ["https://accounts.google.com/v3/signin/identifier?x=1", "https://www.amazon.com/ap/signin?x=1"] {
        assert!(bl::SIGN_IN.is_match(url), "{url}");
    }
    for url in ["https://mail.google.com/mail/u/0/#inbox", "https://news.ycombinator.com/", "https://github.com/notifications"] {
        assert!(!bl::SIGN_IN.is_match(url), "{url}");
    }
}

// ---- which Chrome profile -----------------------------------------------------------------------------------------

const LIST_ACCOUNTS: &str = r#"["gaia.l.a.r",[["gaia.l.a",1,"Guru","owner@work.example","https://x/photo.jpg",1,1,0,null,1,"1"],["gaia.l.a",1,"Guru","OWNER@gmail.com","https://x/p2.jpg",0,0,0,null,1,"2"]]]"#;

fn profiles_map() -> IndexMap<String, &'static str> {
    [("owner@gmail.com", "personal-ctx"), ("owner@work.example", "work-ctx"), ("student@school.example", "school-ctx")]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
}

#[test]
fn test_accounts_are_read_primary_first_and_deduplicated() {
    assert_eq!(bl::accounts_in(LIST_ACCOUNTS), ["owner@work.example", "owner@gmail.com"]);
    assert!(bl::accounts_in("").is_empty() && bl::accounts_in("[]").is_empty());
}

#[test]
fn test_a_request_names_its_profile() {
    for (request, expected) in [
        ("check my work inbox", "owner@work.example"),
        ("open canvas on my school account", "student@school.example"),
        ("how many unread in gmail", "owner@gmail.com"),
        ("use student@school.example and open canvas", "student@school.example"),
        ("what's the weather", ""), // nothing named, no default: the first profile
    ] {
        assert_eq!(bl::profile_for(request, &profiles_map(), ""), expected, "{request}");
    }
}

#[test]
fn test_the_configured_default_applies_only_when_it_is_open() {
    assert_eq!(bl::profile_for("what's the weather", &profiles_map(), "owner@work.example"), "owner@work.example");
    assert_eq!(bl::profile_for("what's the weather", &profiles_map(), "someone@else.com"), "");
    let cfg = bl::configured(Some(&env(&[("EVE_CHROME_PROFILE", " Owner@Work.Example ")])));
    assert_eq!(cfg.chrome_profile, "owner@work.example");
}

static NEXT_SID: AtomicUsize = AtomicUsize::new(0);

/// A Chrome profile: its cookies for Google's account list (a SID the fetch seam maps to `body`).
fn ctx(body: &str, bodies: &Arc<Mutex<HashMap<String, String>>>) -> Arc<dyn ContextHandle> {
    let sid = format!("sid-{}", NEXT_SID.fetch_add(1, Ordering::SeqCst));
    bodies.lock().insert(sid.clone(), body.to_string());
    let mut c = FakeContext::new(FakePage::new("about:blank", "", "", vec![]));
    c.sid = sid;
    Arc::new(c)
}

/// Google's account list answering in `secs`, counting how many requests are in flight at once.
#[derive(Clone)]
struct SlowGoogle {
    secs: f64,
    hang: Vec<String>,
    bodies: Arc<Mutex<HashMap<String, String>>>,
    calls: Arc<AtomicUsize>,
    in_flight: Arc<Mutex<(usize, usize)>>,
}

impl SlowGoogle {
    fn new(secs: f64, hang: &[&str], bodies: &Arc<Mutex<HashMap<String, String>>>) -> SlowGoogle {
        SlowGoogle {
            secs,
            hang: hang.iter().map(|s| s.to_string()).collect(),
            bodies: bodies.clone(),
            calls: Arc::default(),
            in_flight: Arc::default(),
        }
    }

    fn seam(&self) -> bl::FetchAccountsFn {
        let me = self.clone();
        Arc::new(move |_url, header, _timeout| {
            me.calls.fetch_add(1, Ordering::SeqCst);
            {
                let mut f = me.in_flight.lock();
                f.0 += 1;
                f.1 = f.1.max(f.0);
            }
            let body = me.bodies.lock().get(header.trim_start_matches("SID=")).cloned().unwrap_or_default();
            let wait = if me.hang.contains(&body) { 5.0 } else { me.secs };
            std::thread::sleep(Duration::from_secs_f64(wait));
            me.in_flight.lock().0 -= 1;
            Ok(body)
        })
    }

    fn most(&self) -> usize {
        self.in_flight.lock().1
    }
}

fn by_cookie(bodies: &Arc<Mutex<HashMap<String, String>>>) -> bl::FetchAccountsFn {
    let bodies = bodies.clone();
    Arc::new(move |_url, header, _t| Ok(bodies.lock().get(header.trim_start_matches("SID=")).cloned().unwrap_or_default()))
}

fn attached(contexts: Vec<Arc<dyn ContextHandle>>, fetch: bl::FetchAccountsFn) -> bl::LaneCore {
    let mut core = core_with(attach_cfg());
    core.browser = Some(Arc::new(FakeBrowser { contexts, connected: true }));
    core.seams.fetch_accounts = fetch;
    core
}

fn same(a: Option<&Arc<dyn ContextHandle>>, b: &Arc<dyn ContextHandle>) -> bool {
    a.is_some_and(|a| same_context(a, b))
}

#[test]
fn test_profiles_map_primary_accounts_to_contexts_and_a_missing_one_is_refused() {
    let bodies = Arc::default();
    let work = ctx(LIST_ACCOUNTS, &bodies);
    let personal = ctx(r#"[[["x",1,"G","owner@gmail.com"]]]"#, &bodies);
    let mut core = attached(vec![work.clone(), personal.clone()], by_cookie(&bodies));
    let found = core.profiles().unwrap();
    assert_eq!(found.keys().collect::<Vec<_>>(), ["owner@work.example", "owner@gmail.com"]);
    assert!(same(found.get("owner@work.example"), &work) && same(found.get("owner@gmail.com"), &personal));
    assert!(same_context(&core.pick_profile("owner@gmail.com").unwrap(), &personal));
    assert!(same_context(&core.pick_profile("").unwrap(), &work));
    let err = core.pick_profile("student@school.example").err().unwrap();
    assert!(is_attach_error(&err, "no open Chrome window for student@school.example"), "{err}");
}

#[test]
fn test_profile_lookups_run_at_once_and_are_cached_for_the_connection() {
    let bodies = Arc::default();
    let google = SlowGoogle::new(0.3, &[], &bodies);
    let a = ctx(LIST_ACCOUNTS, &bodies);
    let b = ctx(r#"[[["x",1,"G","owner@gmail.com"]]]"#, &bodies);
    let c = ctx(r#"[[["x",1,"G","student@school.example"]]]"#, &bodies);
    let mut core = attached(vec![a.clone(), b.clone(), c.clone()], google.seam());
    let t0 = Instant::now();
    let found = core.profiles().unwrap();
    let secs = t0.elapsed().as_secs_f64();
    assert_eq!(found.keys().collect::<Vec<_>>(), ["owner@work.example", "owner@gmail.com", "student@school.example"]);
    assert!(same(found.get("owner@work.example"), &a) && same(found.get("owner@gmail.com"), &b));
    assert!(same(found.get("student@school.example"), &c));
    assert_eq!(google.most(), 3, "lookups were not concurrent");
    assert!(secs < 0.6, "three 0.3 s lookups took {secs:.2} s: one after another");
    let again = core.profiles().unwrap(); // cached: no second round
    assert_eq!(again.keys().collect::<Vec<_>>(), found.keys().collect::<Vec<_>>());
    assert_eq!(google.calls.load(Ordering::SeqCst), 3);
    core.close(); // a new connection asks again
    core.browser = Some(Arc::new(FakeBrowser { contexts: vec![a, b, c], connected: true }));
    assert!(core.profile_map.is_none());
}

#[test]
fn test_a_profile_that_does_not_answer_costs_only_the_lookup_budget() {
    let bodies = Arc::default();
    let silent = r#"[[["x",1,"G","slow@example.org"]]]"#;
    let google = SlowGoogle::new(0.01, &[silent], &bodies);
    let work = ctx(LIST_ACCOUNTS, &bodies);
    let slow = ctx(silent, &bodies);
    let mut core = attached(vec![work.clone(), slow], google.seam());
    core.seams.profile_lookup_budget_secs = 0.3;
    let t0 = Instant::now();
    let found = core.profiles().unwrap();
    assert_eq!(found.keys().collect::<Vec<_>>(), ["owner@work.example"]);
    assert!(same(found.get("owner@work.example"), &work));
    assert!(t0.elapsed().as_secs_f64() < 1.0);
}

#[test]
fn test_with_one_chrome_profile_there_is_nothing_to_look_up() {
    let bodies = Arc::default();
    let google = SlowGoogle::new(0.0, &[], &bodies);
    let only = ctx(LIST_ACCOUNTS, &bodies);
    let mut core = attached(vec![only.clone()], google.seam());
    core.context = Some(only);
    core.page = Some(bl::Page::new(FakePage::new("https://x/", "eve", "", vec![]).boxed()));
    assert_eq!(core.use_profile("check my work inbox").unwrap(), "");
    assert_eq!(google.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn test_a_lost_tab_in_the_owners_chrome_is_replaced_by_a_new_one_never_theirs() {
    let owners = vec![FakePage::new("https://inbox/", "owner's inbox", "", vec![]), FakePage::new("https://bank/", "owner's bank", "", vec![])];
    let fresh = FakePage::new("about:blank", "eve's new tab", "", vec![]);
    let mut c = FakeContext::new(owners[0].clone());
    c.pages = owners;
    c.fresh = Some(fresh);
    let opened = c.opened.clone();
    let ctx: Arc<dyn ContextHandle> = Arc::new(c);
    let mut core = core_with(attach_cfg());
    core.browser = Some(Arc::new(FakeBrowser { contexts: vec![], connected: true }));
    core.context = Some(ctx);
    let old = FakePage::new("https://old/", "eve's old tab", "", vec![]);
    old.state().closed = true;
    core.page = Some(bl::Page::new(old.boxed()));
    let got = core.ensure().unwrap();
    assert_eq!(got.page.title().unwrap(), "eve's new tab");
    assert_eq!(*opened.lock(), ["new"]);
}

fn counting_run(counts: &[&str], calls: &Arc<Mutex<Vec<String>>>) -> impl Fn(&[String], f64) -> anyhow::Result<String> {
    let counts = Arc::new(Mutex::new(counts.iter().map(|s| s.to_string()).collect::<Vec<_>>()));
    let calls = calls.clone();
    move |argv: &[String], _t: f64| {
        let script = argv.last().unwrap();
        calls.lock().push(if script.contains("make new window") { "make" } else { "count" }.into());
        Ok(if script.contains("count windows") { counts.lock().remove(0) } else { String::new() })
    }
}

#[test]
fn test_chrome_with_no_window_gets_one_before_connecting() {
    let calls = Arc::default();
    let run = counting_run(&["0", "1"], &calls);
    assert!(bl::ensure_chrome_window_with(&run, &|_| {}).unwrap());
    assert_eq!(*calls.lock(), ["count", "make", "count"]);
}

#[test]
fn test_chrome_with_a_window_is_left_alone() {
    let calls: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = calls.clone();
    let run = move |argv: &[String], _t: f64| {
        seen.lock().push(argv.last().unwrap().clone());
        Ok("2".to_string())
    };
    assert!(bl::ensure_chrome_window_with(&run, &|_| {}).unwrap());
    let calls = calls.lock();
    assert!(calls.len() == 1 && !calls[0].contains("make new window"));
}

fn own_chrome_line() -> String {
    format!("{} --user-data-dir=/Users/me/.config/eve-bridge/chrome --remote-debugging-port=0 --no-first-run", bl::CHROME_BINARY)
}

const HELPER: &str =
    "/Applications/Google Chrome.app/Contents/Frameworks/x/Helpers/Google Chrome Helper.app/Contents/MacOS/Google Chrome Helper";

fn ps_run(tables: Vec<String>, calls: &Arc<Mutex<Vec<Vec<String>>>>) -> impl Fn(&[String], f64) -> anyhow::Result<String> {
    let rows = Arc::new(Mutex::new(tables));
    let calls = calls.clone();
    move |argv: &[String], _t: f64| {
        calls.lock().push(argv.to_vec());
        Ok(if argv[0] == "ps" { rows.lock().remove(0) } else { String::new() })
    }
}

#[test]
fn test_only_eves_own_chrome_running_starts_the_owners_by_path() {
    let calls = Arc::default();
    let own = own_chrome_line();
    let run = ps_run(vec![format!("{own}\n{HELPER}"), own.clone(), format!("{own}\n{}", bl::CHROME_BINARY)], &calls);
    assert!(bl::ensure_owner_chrome_with(&run, &|_| {}, 10.0, 3.0).unwrap());
    assert!(calls.lock().contains(&vec!["open".to_string(), "-na".into(), bl::CHROME_APP.into()]));
}

#[test]
fn test_the_owners_chrome_already_running_is_left_alone() {
    let calls = Arc::default();
    let run = ps_run(vec![format!("{}\n{} --flag-switches-begin", own_chrome_line(), bl::CHROME_BINARY)], &calls);
    assert!(bl::ensure_owner_chrome_with(&run, &|_| {}, 10.0, 3.0).unwrap());
    assert!(calls.lock().iter().all(|c| c[0] == "ps"));
}

#[test]
fn test_helpers_and_eves_chrome_do_not_count_as_the_owners() {
    assert!(!bl::owner_chrome_running_with(&ps_run(vec![format!("{}\n{HELPER}", own_chrome_line())], &Arc::default())).unwrap());
    let default = format!("{} --user-data-dir={}", bl::CHROME_BINARY, eve_steel_local::py_path(bl::DEFAULT_CHROME_DIR).display());
    assert!(bl::owner_chrome_running_with(&ps_run(vec![default], &Arc::default())).unwrap());
}

#[test]
fn test_chrome_that_never_starts_reports_false() {
    let own = own_chrome_line();
    let run = move |argv: &[String], _t: f64| Ok(if argv[0] == "ps" { own.clone() } else { String::new() });
    assert!(!bl::ensure_owner_chrome_with(&run, &|_| {}, 0.01, 3.0).unwrap());
}

// -- eve's own Chrome (EVE_BROWSER_OWN) ----------------------------------------------------------------------------

#[test]
fn test_own_chrome_is_off_unless_asked_and_turns_the_lane_on_by_itself() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(!bl::configured(Some(&env(&[]))).own);
    let cfg = bl::configured(Some(&env(&[("EVE_BROWSER_OWN", "1"), ("EVE_OWN_CHROME_DIR", tmp.path().to_str().unwrap())])));
    assert!(cfg.own && cfg.attach && cfg.own_dir == tmp.path());
}

#[test]
fn test_own_chrome_starts_on_its_own_folder_never_the_owners() {
    let cmd = bl::own_chrome_command(Path::new("/x/eve-chrome"), "https://accounts.google.com/");
    assert_eq!(cmd[..3], ["open", "-na", "Google Chrome"]);
    assert!(cmd.contains(&"--user-data-dir=/x/eve-chrome".to_string()) && cmd.contains(&"--remote-debugging-port=0".to_string()));
    assert_eq!(cmd.last().unwrap(), "https://accounts.google.com/");
    assert!(!cmd.iter().any(|a| a.contains("Application Support/Google/Chrome")));
}

fn no_run() -> impl Fn(&[String], f64) -> anyhow::Result<String> {
    |_: &[String], _: f64| Ok(String::new())
}

#[test]
fn test_a_running_own_chrome_is_reused_without_starting_another() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("DevToolsActivePort"), "51234\n/devtools/browser/abc\n").unwrap();
    let ran: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
    let seen = ran.clone();
    let run = move |argv: &[String], _t: f64| {
        seen.lock().push(argv.to_vec());
        Ok(String::new())
    };
    let got = bl::start_own_chrome_with(tmp.path(), "", &run, bl::OWN_START_SECS, 0.25, &|p| p == 51234, &|_| false).unwrap();
    assert_eq!(got, "ws://127.0.0.1:51234/devtools/browser/abc");
    assert!(ran.lock().is_empty());
}

#[test]
fn test_a_stale_port_file_is_not_believed_and_chrome_is_started() {
    let tmp = tempfile::tempdir().unwrap();
    let dir: PathBuf = tmp.path().into();
    std::fs::write(dir.join("DevToolsActivePort"), "40000\n/devtools/browser/old\n").unwrap(); // a Chrome that has quit
    let ran: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
    let (seen, d) = (ran.clone(), dir.clone());
    let run = move |argv: &[String], _t: f64| {
        seen.lock().push(argv.to_vec());
        assert!(!d.join("DevToolsActivePort").exists()); // the stale file went first
        std::fs::write(d.join("DevToolsActivePort"), "51234\n/devtools/browser/new\n").unwrap();
        Ok(String::new())
    };
    let got = bl::start_own_chrome_with(&dir, "", &run, bl::OWN_START_SECS, 0.0, &|p| p == 51234, &|_| false).unwrap();
    assert_eq!(got, "ws://127.0.0.1:51234/devtools/browser/new");
    let ran = ran.lock();
    assert!(ran.len() == 1 && ran[0].contains(&format!("--user-data-dir={}", dir.display())));
}

#[test]
fn test_own_chrome_that_never_comes_up_is_an_attach_error() {
    let tmp = tempfile::tempdir().unwrap();
    let err = bl::start_own_chrome_with(tmp.path(), "", &no_run(), 0.05, 0.01, &|_| false, &|_| false).unwrap_err();
    assert!(is_attach_error(&err, "did not start"), "{err}");
}

#[test]
fn test_own_chrome_never_waits_on_an_allow_dialog() {
    let page = FakePage::new("about:blank", "", "", vec![]);
    let mut core = core_on(&page, tempfile::tempdir().unwrap().path());
    core.config = bl::BrowserLaneConfig { attach: true, own: true, ..core.config.clone() };
    let lane = bl::BrowserLane::from_core(core).unwrap();
    let asked: Arc<Mutex<Vec<String>>> = Arc::default();
    let a = asked.clone();
    run(lane.connect_answering(move || async move {
        a.lock().push("asked".into());
    }))
    .unwrap();
    assert!(lane.connected() && asked.lock().is_empty());
    run(lane.close()).unwrap();
}

#[test]
fn test_own_chrome_uses_its_one_profile_without_a_lookup() {
    let mut core = core_with(bl::BrowserLaneConfig {
        attach: true,
        own: true,
        chrome_profile: "someone@example.com".into(),
        ..Default::default()
    });
    let first: Arc<dyn ContextHandle> = Arc::new(FakeContext::new(FakePage::new("about:blank", "", "", vec![])));
    let second: Arc<dyn ContextHandle> = Arc::new(FakeContext::new(FakePage::new("about:blank", "", "", vec![])));
    core.browser = Some(Arc::new(FakeBrowser { contexts: vec![first.clone(), second], connected: true }));
    core.seams.profiles = Some(Arc::new(|| panic!("no profile lookup in eve's own Chrome")));
    assert!(same_context(&core.pick_profile("someone@example.com").unwrap(), &first));
}

#[test]
fn test_signing_in_opens_eves_chrome_with_no_port() {
    let cmd = bl::sign_in_command(Path::new("/x/eve-chrome"), "https://accounts.google.com/");
    assert!(cmd.contains(&"--user-data-dir=/x/eve-chrome".to_string()) && cmd.last().unwrap() == "https://accounts.google.com/");
    assert!(!cmd.iter().any(|a| a.starts_with("--remote-debugging")));
}

#[test]
fn test_a_sign_in_chrome_left_open_says_to_quit_it() {
    let tmp = tempfile::tempdir().unwrap();
    let err = bl::start_own_chrome_with(tmp.path(), "", &no_run(), 0.05, 0.01, &|_| false, &|_| true).unwrap_err();
    assert!(is_attach_error(&err, "open for signing in"), "{err}");
}

#[test]
fn test_own_chrome_running_looks_for_its_own_folder_only() {
    let seen: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
    let s = seen.clone();
    let run = move |argv: &[String], _t: f64| {
        s.lock().push(argv.to_vec());
        Ok(String::new())
    };
    assert!(!bl::own_chrome_running_with(Path::new("/x/eve-chrome"), &run).unwrap());
    assert_eq!(*seen.lock(), [["pgrep", "-f", "--", "--user-data-dir=/x/eve-chrome"]]);
}

// ---- an attach that fails after connecting (test_browser_lane.py) ------------------------------------------------

#[test]
fn test_an_attach_that_fails_after_connecting_closes_the_driver() {
    let tmp = tempfile::tempdir().unwrap();
    let stops: Arc<Mutex<Vec<Arc<std::sync::atomic::AtomicBool>>>> = Arc::default();
    let mut core = core_with(bl::BrowserLaneConfig {
        profile: tmp.path().join("p"),
        attach: true,
        chrome_dir: tmp.path().into(),
        chrome_profile: "someone@example.com".into(),
        ..Default::default()
    });
    let made = stops.clone();
    core.seams.start_driver = Arc::new(move || {
        let ctx: Arc<dyn ContextHandle> = Arc::new(FakeContext::new(FakePage::new("about:blank", "", "", vec![])));
        let d = FakeDriver {
            stopped: Arc::default(),
            browser: Some(Arc::new(FakeBrowser { contexts: vec![ctx], connected: true })),
            endpoints: Arc::default(),
            log: None,
        };
        made.lock().push(d.stopped.clone());
        Ok(Box::new(d) as Box<dyn Driver>)
    });
    core.seams.devtools_endpoint = Arc::new(|_, _| Some("ws://127.0.0.1:9/devtools/browser/x".into()));
    core.seams.ensure_chrome_window = Arc::new(|| Ok(true));
    core.seams.profiles = Some(Arc::new(IndexMap::new)); // no window open in that profile
    for _ in 0..2 {
        let err = core.ensure().err().unwrap();
        assert!(err.downcast_ref::<bl::AttachError>().is_some(), "{err}");
        assert!(stops.lock().last().unwrap().load(Ordering::SeqCst));
        assert!(core.pw.is_none() && core.browser.is_none() && core.context.is_none());
    }
    let stops = stops.lock();
    assert!(stops.len() == 2 && stops.iter().all(|s| s.load(Ordering::SeqCst))); // never two drivers alive at once
}

// ---- Steel (test_steel_local.py, the lane) --------------------------------------------------------------------

#[test]
fn test_steel_is_off_unless_asked() {
    let c = bl::configured(Some(&env(&[])));
    assert!(!c.steel);
}

#[test]
fn test_steel_turns_the_lane_on_and_wins_over_own_and_attach() {
    let c = bl::configured(Some(&env(&[("EVE_BROWSER_STEEL", "1"), ("EVE_BROWSER_OWN", "1"), ("EVE_BROWSER_ATTACH", "1")])));
    assert!(c.steel && c.attach && !c.own);
    assert!(bl::configured(Some(&env(&[("EVE_BROWSER_OWN", "1")]))).own); // own mode is unchanged without it
}

#[test]
fn test_the_lane_attaches_to_the_steel_session() {
    let page = FakePage::new("about:blank", "t", "", vec![]);
    let mut c = FakeContext::new(page.clone());
    c.fresh = Some(page.clone());
    let ctx: Arc<dyn ContextHandle> = Arc::new(c);
    let endpoints: Arc<Mutex<Vec<String>>> = Arc::default();
    let mut core = core_with(bl::BrowserLaneConfig { attach: true, steel: true, ..Default::default() });
    let (e, cx) = (endpoints.clone(), ctx.clone());
    core.seams.start_driver = Arc::new(move || {
        Ok(Box::new(FakeDriver {
            stopped: Arc::default(),
            browser: Some(Arc::new(FakeBrowser { contexts: vec![cx.clone()], connected: true })),
            endpoints: e.clone(),
            log: None,
        }) as Box<dyn Driver>)
    });
    core.seams.steel_start = Arc::new(|| Ok("ws://127.0.0.1:3000/".into()));
    let title = core.ensure().unwrap().page.title().unwrap();
    assert_eq!(*endpoints.lock(), ["ws://127.0.0.1:3000/"]);
    assert_eq!(title, "t");
    assert!(same_context(core.context.as_ref().unwrap(), &ctx));
}

#[test]
fn test_steel_not_reachable_is_an_attach_error_with_its_reason() {
    let mut core = core_with(bl::BrowserLaneConfig { attach: true, steel: true, ..Default::default() });
    core.seams.steel_start =
        Arc::new(|| Err(eve_steel_local::SteelError("Steel did not answer within 40 s".into()).into()));
    let err = core.ensure().err().unwrap();
    assert!(is_attach_error(&err, "did not answer"), "{}", error_message(&err));
}

#[test]
fn test_closing_the_steel_lane_closes_eves_tab_and_releases_the_session() {
    let calls: Arc<Mutex<Vec<String>>> = Arc::default();
    let mut core = attached_with_log(bl::BrowserLaneConfig { attach: true, steel: true, ..Default::default() }, &calls);
    let c = calls.clone();
    core.seams.steel_release = Arc::new(move || c.lock().push("release".into()));
    core.close();
    assert_eq!(*calls.lock(), ["eve tab.close", "playwright.stop", "release"]);
}

#[test]
fn test_closing_eves_own_chrome_never_touches_steel() {
    let calls: Arc<Mutex<Vec<String>>> = Arc::default();
    let mut core = core_with(bl::BrowserLaneConfig { attach: true, own: true, ..Default::default() });
    core.browser = Some(Arc::new(FakeBrowser { contexts: vec![], connected: true }));
    core.pw = Some(Box::new(FakeDriver { stopped: Arc::default(), browser: None, endpoints: Arc::default(), log: None }));
    let c = calls.clone();
    core.seams.steel_release = Arc::new(move || c.lock().push("release".into()));
    core.close();
    assert!(calls.lock().is_empty());
}

// ---- connect (test_chrome_lane.py) -------------------------------------------------------------------------------

#[test]
fn test_a_dead_connection_is_replaced_on_the_next_connect() {
    let events: Arc<Mutex<Vec<String>>> = Arc::default();
    let tab = FakePage::new("https://x/", "eve", "", vec![]);
    tab.state().close_log = Some(events.clone()); // "eve tab.close": the dead connection closing
    let fresh = FakePage::new("about:blank", "new", "", vec![]);
    let ev = events.clone();
    let cfg = bl::BrowserLaneConfig { attach: true, ..Default::default() };
    let mut core = bl::LaneCore::new(
        cfg,
        bl::LaneOptions {
            launcher: Some(Box::new(move || {
                ev.lock().push("connected".into());
                Ok(Arc::new(FakeContext::new(fresh.clone())) as Arc<dyn ContextHandle>)
            })),
            vault: no_vault(),
            ..Default::default()
        },
    )
    .unwrap();
    core.context = Some(Arc::new(FakeContext::new(tab.clone())));
    core.browser = Some(Arc::new(FakeBrowser { contexts: vec![], connected: false }));
    core.page = Some(bl::Page::new(tab.boxed()));
    let lane = bl::BrowserLane::from_core(core).unwrap();
    let prompt = |events: Arc<Mutex<Vec<String>>>| move || async move { events.lock().push("asked".into()) };
    run(lane.connect_answering(prompt(events.clone()))).unwrap();
    // the dead one closes first; the new connection and the owner's consent then run side by side
    {
        let mut ev = events.lock();
        assert_eq!(ev[0], "eve tab.close", "{ev:?}");
        let mut rest = ev[1..].to_vec();
        rest.sort();
        assert_eq!(rest, ["asked", "connected"]);
        ev.clear();
    }
    run(lane.run(|c| c.browser = Some(Arc::new(FakeBrowser { contexts: vec![], connected: true })))).unwrap();
    run(lane.connect_answering(prompt(events.clone()))).unwrap();
    assert!(events.lock().is_empty()); // alive: no reconnect, no new question
}

#[test]
fn test_closed_lane_refuses_new_work() {
    let page = FakePage::new("about:blank", "", "", vec![]);
    let lane = lane_on(&page, tempfile::tempdir().unwrap().path());
    run(lane.close()).unwrap();
    let err = run(lane.outline()).unwrap_err();
    assert!(error_message(&err).contains("cannot schedule new futures after shutdown"));
}

#[test]
fn test_the_phone_wait_ends_before_the_connection_gives_up() {
    // chrome_consent.ASK_TIMEOUT_SECS is 100 s (test_chrome_consent.py); the lane waits longer.
    const { assert!(100 * 1000 < bl::CONSENT_TIMEOUT_MS) };
}

#[test]
fn test_plans_started_counts_every_plan() {
    let tmp = tempfile::tempdir().unwrap();
    let page = FakePage::new("https://shop.example/", "Eve Shop", "Welcome", vec![button("0", "Search")]);
    let lane = lane_on(&page, tmp.path());
    let plan = serde_json::json!({"steps": [{"kind": "click", "target": "the Search button", "label_hint": "Search"}],
                                  "final_say": "Searched.", "success": null});
    run(closing(&lane, lane.run_plan(plan, "click search on the shop site", 0, no_approvals(), false))).unwrap();
    assert_eq!(lane.plans_started.load(Ordering::SeqCst), 1);
}
