//! Port of test_click_recheck.py, test_click_recheck_tolerance.py and the browser half of
//! test_side_effect_links.py / test_side_effect_precision.py: a click is refused when the control under its id
//! is no longer the one the decision was made on; what a live page changes on its own is tolerated. The last
//! tests run the lane's real COLLECT_JS and RECHECK_JS in node over a small DOM stand-in.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::*;
use eve_browser_lane as bl;
use eve_browser_lane::driver::{PageHandle, SelectBy};
use eve_desktop_lanes::ax_candidates::needs_confirmation;
use eve_desktop_lanes::{casefold, py_collapse, LaneError};
use parking_lot::Mutex;
use serde_json::{json, Value};

fn news_page() -> FakePage {
    FakePage::new(
        "https://news.example/",
        "News",
        "Top stories today",
        vec![
            link("0", "Top stories", "https://news.example/top"),
            button("1", "Subscribe"),
            link("2", "World", "https://news.example/world"),
        ],
    )
}

fn set_field(eid: &'static str, key: &'static str, value: &'static str) -> impl FnOnce(&mut FakeState) + Send {
    move |st| {
        st.element(eid).unwrap().insert(key.into(), json!(value));
    }
}

#[test]
fn test_a_changed_control_is_refused_and_not_clicked() {
    type M = Box<dyn FnOnce(&mut FakeState) + Send>;
    let cases: Vec<(M, &str)> = vec![
        (Box::new(set_field("0", "role", "button")), "role"),
        (Box::new(set_field("0", "name", "Sponsored")), "name"),
        (Box::new(set_field("0", "href", "https://ads.example/x")), "href"),
        (Box::new(|st: &mut FakeState| st.url = "https://news.example/other".into()), "page address"),
        (Box::new(|st: &mut FakeState| st.doc += 7.0), "document"),
        (Box::new(|st: &mut FakeState| st.elements[0] = button("0", "Unsubscribe")), "role"),
    ];
    for (mutate, what) in cases {
        let page = news_page();
        let p = bl::Page::new(page.boxed());
        let snap = p.snapshot().unwrap();
        let target = snap.elements.iter().find(|c| c.label == "Top stories").unwrap();
        page.state().mutate = Some(mutate);
        let said = p.click_candidate(target);
        assert!(said.starts_with(bl::STALE_PREFIX), "{said}");
        assert!(said.contains(what), "{said}");
        assert!(page.clicks().is_empty());
        assert_eq!(page.state().rechecks, 1);
    }
}

#[test]
fn test_a_control_that_vanished_is_refused() {
    let page = news_page();
    let p = bl::Page::new(page.boxed());
    let snap = p.snapshot().unwrap();
    let target = snap.elements.iter().find(|c| c.label == "Top stories").unwrap();
    page.set_mutate(|st| st.elements.retain(|e| id_of(e) != "0"));
    let said = p.click_candidate(target);
    assert!(said.starts_with("refused:") && page.clicks().is_empty());
}

#[test]
fn test_an_unchanged_control_is_clicked_even_when_other_text_moved() {
    let page = news_page();
    let p = bl::Page::new(page.boxed());
    let snap = p.snapshot().unwrap();
    let target = snap.elements.iter().find(|c| c.label == "Top stories").unwrap();
    page.set_mutate(|st| {
        st.body.push_str(" Breaking: more news");
        st.element("2").unwrap().insert("name".into(), json!("World (3 new)"));
    });
    let said = p.click_candidate(target);
    assert!(said.starts_with("clicked link 'Top stories'"), "{said}");
    assert_eq!(page.clicks(), vec!["0"]);
    assert_eq!(page.state().rechecks, 1);
}

#[test]
fn test_an_unreadable_recheck_refuses() {
    let page = news_page();
    let p = bl::Page::new(page.boxed());
    let snap = p.snapshot().unwrap();
    let target = snap.elements.iter().find(|c| c.label == "World").unwrap();
    page.state().broken_recheck = true;
    let said = p.click_candidate(target);
    assert!(said.starts_with(bl::STALE_PREFIX) && page.clicks().is_empty(), "{said}");
}

#[test]
fn test_click_recheck_is_a_pure_comparison() {
    let rec = json!({"url": "u", "doc": "1", "role": "link", "name": "A", "href": "h"});
    let cases: Vec<(Value, Vec<&str>)> = vec![
        (json!({"url": "u", "doc": "1", "found": true, "role": "link", "name": "A", "href": "h"}), vec![]),
        (
            json!({"url": "u2", "doc": "2", "found": true, "role": "button", "name": "B", "href": ""}),
            vec!["page address", "document", "role", "name", "href"],
        ),
        (json!({"url": "u", "doc": "1", "found": false}), vec!["control"]),
        (json!("not a dict"), vec!["control"]),
    ];
    for (now, changed) in cases {
        assert_eq!(bl::click_recheck(&rec, &now), changed, "{now}");
    }
}

#[test]
fn test_the_executor_stops_on_a_stale_click_and_presses_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let page =
        FakePage::new("https://news.example/", "News", "Top stories today", vec![button("0", "Show more"), button("1", "Print")]);
    let lane = lane_on(&page, tmp.path());
    page.set_mutate(set_field("0", "name", "Show less")); // between the decision and the click
    let plan = json!({"steps": [{"kind": "click", "target": "the Show more button", "label_hint": "Show more"}],
                      "final_say": "Shown.", "success": {"kind": "text_visible", "value": "Top stories"}});
    let result =
        run(closing(&lane, lane.run_plan(plan, "show more stories on news.example", 0, no_approvals(), false))).unwrap();
    let status = result["status"].as_str().unwrap();
    assert!(status == "partial" || status == "none", "{result}");
    assert!(page.clicks().is_empty() && page.state().rechecks == 1);
    assert_eq!(result["ledger"][0]["effect"], "refused", "{result}");
}

// ---- test_click_recheck_tolerance.py ------------------------------------------------------------------------

const HREF: &str = "https://news.example/item?id=41";

fn rec(over: Value) -> Value {
    let mut base = json!({"url": "https://news.example/", "doc": "1", "role": "link", "name": "142 points",
                          "href": HREF, "dom_id": ""});
    for (k, v) in over.as_object().unwrap() {
        base[k] = v.clone();
    }
    base
}

fn now(over: Value) -> Value {
    let mut base = rec(json!({}));
    base["found"] = json!(true);
    for (k, v) in over.as_object().unwrap() {
        base[k] = v.clone();
    }
    base
}

#[test]
fn test_tolerated() {
    let cases = [
        (rec(json!({})), now(json!({"url": "https://news.example/#comments"}))),
        (rec(json!({"url": "https://news.example/#top"})), now(json!({"url": "https://news.example/#bottom"}))),
        (rec(json!({})), now(json!({"name": "143 points"}))),
        (rec(json!({"name": "1,204 comments"})), now(json!({"name": "1,205 comments"}))),
        (rec(json!({"name": "3 minutes ago"})), now(json!({"name": "4 minutes ago"}))),
        (rec(json!({"name": "2 hours ago"})), now(json!({"name": "1 day ago"}))),
        (rec(json!({"name": "just now"})), now(json!({"name": "2 minutes ago"}))),
        (rec(json!({"name": "World (3 new)"})), now(json!({"name": "World (4 new)"}))),
        (rec(json!({"name": "12.3K views · 3h"})), now(json!({"name": "12.4K views · 4h"}))),
        (
            rec(json!({"role": "button", "href": "", "dom_id": "like-41", "name": "Like 5"})),
            now(json!({"role": "button", "href": "", "dom_id": "like-41", "name": "Like 6"})),
        ),
    ];
    for (recorded, seen) in cases {
        assert_eq!(bl::click_recheck(&recorded, &seen), Vec::<String>::new(), "{recorded} {seen}");
    }
}

#[test]
fn test_still_refused() {
    let cases = [
        (rec(json!({})), now(json!({"url": "https://news.example/news?p=2"})), "page address"),
        (rec(json!({})), now(json!({"url": "https://news.example/other#comments"})), "page address"),
        (rec(json!({"name": "Upvote"})), now(json!({"name": "Unvote"})), "name"),
        (rec(json!({"name": "Follow"})), now(json!({"name": "Following"})), "name"),
        (rec(json!({"name": "142 points"})), now(json!({"name": "142 points flagged"})), "name"),
        (
            rec(json!({"role": "button", "href": "", "name": "Show 5 more"})),
            now(json!({"role": "button", "href": "", "name": "Show 6 more"})),
            "name",
        ),
        (
            rec(json!({"role": "button", "href": "", "dom_id": "a", "name": "Like 5"})),
            now(json!({"role": "button", "href": "", "dom_id": "b", "name": "Like 6"})),
            "name",
        ),
        (rec(json!({})), now(json!({"name": "143 points", "href": "https://news.example/item?id=42"})), "href"),
        (rec(json!({})), now(json!({"name": "143 points", "role": "button"})), "role"),
        (rec(json!({})), now(json!({"doc": "2"})), "document"),
    ];
    for (recorded, seen, what) in cases {
        assert!(bl::click_recheck(&recorded, &seen).iter().any(|w| w == what), "{recorded} {seen} {what}");
    }
}

#[test]
fn test_a_counter_and_a_fragment_change_do_not_stop_a_click() {
    let page = FakePage::new("https://news.example/", "News", "Top stories", vec![link("0", "142 points", HREF), button("1", "Subscribe")]);
    let p = bl::Page::new(page.boxed());
    let snap = p.snapshot().unwrap();
    let target = snap.elements.iter().find(|c| c.label == "142 points").unwrap();
    page.set_mutate(|st| {
        st.url = "https://news.example/#item-41".into();
        st.element("0").unwrap().insert("name".into(), json!("143 points"));
    });
    let said = p.click_candidate(target);
    assert!(said.starts_with("clicked"), "{said}");
    assert_eq!(page.clicks(), vec!["0"]);
}

#[test]
fn test_a_renamed_control_is_still_refused_through_the_page() {
    let page = FakePage::new("https://news.example/", "News", "Top stories", vec![link("0", "41 comments", HREF)]);
    let p = bl::Page::new(page.boxed());
    let target = p.snapshot().unwrap().elements[0].clone();
    page.set_mutate(set_field("0", "name", "41 comments hidden"));
    assert!(p.click_candidate(&target).starts_with(bl::STALE_PREFIX));
    assert!(page.clicks().is_empty());
}

#[test]
fn test_a_changed_amount_on_an_asks_first_control_is_always_stale() {
    for (before, after) in [
        ("Pay $5", "Pay $5000"),
        ("Buy for 5", "Buy for 500"),
        ("Donate €10", "Donate €100"),
        ("Delete 3 files", "Delete 30 files"),
        ("Place order (2 items)", "Place order (20 items)"),
    ] {
        let recorded = json!({"url": "https://shop.example/cart", "doc": "1", "role": "button", "name": before,
                              "href": "", "dom_id": "pay"});
        let mut seen = recorded.clone();
        seen["found"] = json!(true);
        seen["name"] = json!(after);
        assert!(bl::click_recheck(&recorded, &seen).contains(&"name".to_string()), "{before}");
    }
}

#[test]
fn test_a_count_on_an_ordinary_control_still_folds() {
    let recorded = json!({"url": "https://news.example/", "doc": "1", "role": "link", "name": "142 comments",
                          "href": "/item?id=1", "dom_id": ""});
    let mut seen = recorded.clone();
    seen["found"] = json!(true);
    seen["name"] = json!("150 comments");
    assert!(bl::click_recheck(&recorded, &seen).is_empty());
}

#[test]
fn volatile_free_folds_counts_and_relative_times() {
    assert_eq!(bl::volatile_free("3 minutes ago"), bl::volatile_free("1 day ago"));
    assert_eq!(bl::volatile_free("12.3K views · 3h"), "# views · #");
    assert_ne!(bl::volatile_free("Upvote"), bl::volatile_free("Unvote"));
    assert_eq!(bl::without_fragment(Some(&json!("https://a/b#c"))), "https://a/b");
    assert_eq!(bl::without_fragment(None), "");
}

// ---- the real scripts, in node ------------------------------------------------------------------------------

/// A DOM stand-in just wide enough for IDENTITY_JS, COLLECT_JS and RECHECK_JS. Elements come in as JSON and
/// their attributes (data-eve-id) go back out, so the state carries from one evaluate to the next.
const HARNESS: &str = r#"
const input = JSON.parse(require('fs').readFileSync(0, 'utf8'));
const st = input.state;
class El {
  constructor(d) { Object.assign(this, d); this.attrs = d.attrs || {}; }
  get tagName() { return this.tag.toUpperCase(); }
  getAttribute(k) { return k in this.attrs ? this.attrs[k] : null; }
  hasAttribute(k) { return k in this.attrs; }
  setAttribute(k, v) { this.attrs[k] = String(v); }
  removeAttribute(k) { delete this.attrs[k]; }
  get href() { return this.attrs.href || ''; }
  get id() { return this.attrs.id || ''; }
  get innerText() { return this.text || ''; }
  get value() { return this.attrs.value; }
  get placeholder() { return this.attrs.placeholder || ''; }
  get title() { return this.attrs.title || ''; }
  get type() { return this.attrs.type || ''; }
  get disabled() { return 'disabled' in this.attrs; }
  get isContentEditable() { return false; }
  get labels() { return []; }
  getBoundingClientRect() { const [x, y, w, h] = this.rect || [10, 10, 100, 20];
    return {left: x, top: y, width: w, height: h, right: x + w, bottom: y + h}; }
  closest() { return null; }
  querySelector() { return null; }
}
const els = st.elements.map(d => new El(d));
global.window = {innerHeight: 800, innerWidth: 1200};
global.location = {href: st.url};
global.performance = {timeOrigin: st.doc};
global.getComputedStyle = () => ({visibility: 'visible', display: 'block', opacity: '1'});
global.document = {
  title: st.title, body: {innerText: st.body}, activeElement: null,
  getElementById: () => null,
  querySelectorAll: (sel) => sel === '[data-eve-id]' ? els.filter(e => e.hasAttribute('data-eve-id'))
                           : sel.startsWith('dialog') ? [] : els,
  querySelector: (sel) => { const m = /data-eve-id="([^"]*)"/.exec(sel);
    return m ? (els.find(e => e.getAttribute('data-eve-id') === m[1]) || null) : null; },
};
const fn = eval('(' + input.script + ')');
const result = fn(input.arg);
process.stdout.write(JSON.stringify({result, attrs: els.map(e => e.attrs)}));
"#;

fn node() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join("node")).find(|p| p.is_file())
}

type NodeMutate = Box<dyn FnOnce(&mut Value) + Send>;

struct NodeState {
    state: Value,
    clicks: Vec<String>,
    mutate: Option<NodeMutate>,
    ran: Vec<String>,
}

/// A page whose evaluate runs the lane's script in node over `state`. Clicks are recorded, not run.
#[derive(Clone)]
struct NodePage {
    node: PathBuf,
    harness: PathBuf,
    inner: Arc<Mutex<NodeState>>,
}

impl NodePage {
    fn new(node: PathBuf, tmp: &Path, url: &str, elements: Value) -> NodePage {
        let harness = tmp.join("harness.js");
        std::fs::write(&harness, HARNESS).unwrap();
        let state = json!({"url": url, "doc": 1000.5, "title": "News", "body": "Top stories", "elements": elements});
        NodePage { node, harness, inner: Arc::new(Mutex::new(NodeState { state, clicks: vec![], mutate: None, ran: vec![] })) }
    }

    fn eid(selector: &str) -> String {
        selector.split('"').nth(1).unwrap_or("").to_string()
    }
}

/// The element whose data-eve-id is `eid`.
fn el<'a>(state: &'a mut Value, eid: &str) -> &'a mut Value {
    state["elements"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|d| d["attrs"]["data-eve-id"] == eid)
        .unwrap()
}

fn unsupported() -> anyhow::Error {
    LaneError::err("AttributeError", "'NodePage' has no such method")
}

impl PageHandle for NodePage {
    fn evaluate(&self, script: &str, arg: Option<&Value>) -> anyhow::Result<Value> {
        use std::io::Write;
        let mut inner = self.inner.lock();
        let what = if script == bl::COLLECT_JS {
            "collect"
        } else if script == bl::RECHECK_JS {
            "recheck"
        } else {
            "?"
        };
        inner.ran.push(what.into());
        let input = json!({"state": inner.state, "script": script, "arg": arg.cloned().unwrap_or(Value::Null)});
        let mut child = std::process::Command::new(&self.node)
            .arg(&self.harness)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()?;
        child.stdin.take().unwrap().write_all(input.to_string().as_bytes())?;
        let out = child.wait_with_output()?;
        assert!(out.status.success(), "node failed");
        let got: Value = serde_json::from_slice(&out.stdout)?;
        let attrs = got["attrs"].as_array().unwrap().clone();
        let elements = inner.state["elements"].as_array_mut().unwrap();
        assert_eq!(elements.len(), attrs.len());
        for (d, a) in elements.iter_mut().zip(attrs) {
            d["attrs"] = a;
        }
        Ok(got["result"].clone())
    }

    fn title(&self) -> anyhow::Result<String> {
        Ok("News".into())
    }

    fn url(&self) -> String {
        self.inner.lock().state["url"].as_str().unwrap_or("").into()
    }

    fn inner_text(&self, _s: &str, _t: u64) -> anyhow::Result<String> {
        Err(unsupported())
    }

    fn count(&self, selector: &str) -> anyhow::Result<usize> {
        let eid = Self::eid(selector);
        let inner = self.inner.lock();
        Ok(inner.state["elements"].as_array().unwrap().iter().filter(|d| d["attrs"]["data-eve-id"] == eid.as_str()).count())
    }

    fn scroll_into_view_if_needed(&self, _s: &str, _t: u64) -> anyhow::Result<()> {
        let mut inner = self.inner.lock();
        if let Some(f) = inner.mutate.take() {
            f(&mut inner.state);
        }
        Ok(())
    }

    fn click(&self, selector: &str, _t: u64) -> anyhow::Result<()> {
        self.inner.lock().clicks.push(Self::eid(selector));
        Ok(())
    }

    fn fill(&self, _s: &str, _v: &str, _t: u64) -> anyhow::Result<()> {
        Err(unsupported())
    }

    fn select_option(&self, _s: &str, _c: SelectBy<'_>, _t: u64) -> anyhow::Result<()> {
        Err(unsupported())
    }

    fn evaluate_on(&self, _s: &str, _script: &str) -> anyhow::Result<Value> {
        Err(unsupported())
    }

    fn keyboard_type(&self, _t: &str, _d: u64) -> anyhow::Result<()> {
        Err(unsupported())
    }

    fn keyboard_press(&self, _k: &str) -> anyhow::Result<()> {
        Err(unsupported())
    }

    fn wait_for_timeout(&self, _ms: u64) {}

    fn wait_for_load_state(&self, _s: &str, _t: u64) -> anyhow::Result<()> {
        Ok(())
    }

    fn goto(&self, _u: &str, _w: &str, _t: u64) -> anyhow::Result<()> {
        Err(unsupported())
    }

    fn bring_to_front(&self) -> anyhow::Result<()> {
        Ok(())
    }

    fn close(&self) -> anyhow::Result<()> {
        Ok(())
    }

    fn screenshot_jpeg(&self, _q: u8, _m: Option<(&str, &str)>) -> anyhow::Result<Vec<u8>> {
        Err(unsupported())
    }
}

fn hn_elements() -> Value {
    json!([{"tag": "a", "attrs": {"href": HREF}, "text": "142 points", "rect": [10, 10, 80, 18]},
           {"tag": "a", "attrs": {"href": "https://news.example/from?site=example.com"}, "text": "example.com",
            "rect": [10, 40, 60, 18]},
           {"tag": "button", "attrs": {"id": "replies-41"}, "text": "5 replies", "rect": [10, 70, 60, 18]}])
}

fn node_click(node: &Path, tmp: &Path, label: &str, mutate: impl FnOnce(&mut Value) + Send + 'static) -> (String, NodePage) {
    let page = NodePage::new(node.to_path_buf(), tmp, "https://news.example/", hn_elements());
    let p = bl::Page::new(Box::new(page.clone()));
    let snap = p.snapshot().unwrap();
    let target = snap.elements.iter().find(|c| c.label == label).unwrap_or_else(|| panic!("no {label}: {snap:?}"));
    page.inner.lock().mutate = Some(Box::new(mutate));
    (p.click_candidate(target), page)
}

#[test]
fn test_real_scripts_read_an_unchanged_control_as_the_same() {
    let Some(node) = node() else {
        eprintln!("skipped: node is not installed");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let (said, page) = node_click(&node, tmp.path(), "example.com", |_| {});
    assert!(said.starts_with("clicked"), "{said}");
    let inner = page.inner.lock();
    assert_eq!(inner.ran, ["collect", "recheck"]);
    assert!(!inner.clicks.is_empty());
}

#[test]
fn test_real_scripts_tolerate_a_counter_and_a_fragment() {
    let Some(node) = node() else {
        eprintln!("skipped: node is not installed");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let live = |st: &mut Value| {
        st["url"] = json!("https://news.example/#item-41");
        el(st, "0")["text"] = json!("143 points");
        el(st, "2")["text"] = json!("6 replies");
    };
    let (said, _) = node_click(&node, tmp.path(), "142 points", live);
    assert!(said.starts_with("clicked"), "{said}");
    let (said, _) = node_click(&node, tmp.path(), "5 replies", live); // a button: tolerated by its unchanged DOM id
    assert!(said.starts_with("clicked"), "{said}");
}

#[test]
fn test_real_scripts_still_refuse_a_changed_control() {
    let Some(node) = node() else {
        eprintln!("skipped: node is not installed");
        return;
    };
    type M = Box<dyn FnOnce(&mut Value) + Send>;
    let cases: Vec<(&str, M, &str)> = vec![
        ("example.com", Box::new(|st: &mut Value| el(st, "1")["text"] = json!("Sponsored")), "name"),
        ("example.com", Box::new(|st: &mut Value| el(st, "1")["attrs"]["href"] = json!("https://ads.example/x")), "href"),
        ("142 points", Box::new(|st: &mut Value| st["url"] = json!("https://news.example/news?p=2")), "page address"),
        ("142 points", Box::new(|st: &mut Value| st["doc"] = json!(2000.25)), "document"),
        (
            "5 replies",
            Box::new(|st: &mut Value| {
                el(st, "2")["attrs"]["id"] = json!("replies-42");
                el(st, "2")["text"] = json!("6 replies");
            }),
            "name",
        ),
        ("5 replies", Box::new(|st: &mut Value| el(st, "2")["text"] = json!("5 replies hidden")), "name"),
        ("example.com", Box::new(|st: &mut Value| el(st, "1")["tag"] = json!("button")), "role"),
    ];
    for (label, mutate, what) in cases {
        let tmp = tempfile::tempdir().unwrap();
        let (said, page) = node_click(&node, tmp.path(), label, mutate);
        assert!(said.starts_with(bl::STALE_PREFIX) && said.contains(what), "{label} {what}: {said}");
        assert!(page.inner.lock().clicks.is_empty());
    }
}

// ---- the browser's own click asks for account changes, href included (test_side_effect_links.py) ------------

#[test]
fn test_the_browser_click_refuses_an_upvote_link_without_a_yes() {
    let page = FakePage::new(
        "https://news.ycombinator.com/",
        "Hacker News",
        "Top stories",
        vec![
            link("0", "Show HN: a thing", "https://news.ycombinator.com/item?id=1"),
            link("1", "", "https://news.ycombinator.com/vote?id=1&how=up"),
            link("2", "upvote", "https://news.ycombinator.com/x"),
        ],
    );
    let p = bl::Page::new(page.boxed());
    let snap = p.snapshot().unwrap();
    let by_id = |id: &str| snap.elements.iter().find(|c| c.id == id).cloned();
    for eid in ["1", "2"] {
        if let Some(c) = by_id(eid) {
            assert!(p.click_candidate(&c).starts_with("refused:"), "{eid}");
        }
    }
    assert!(page.clicks().is_empty());
    assert!(p.click_candidate(&by_id("0").unwrap()).starts_with("clicked "));
    let upvote = by_id("2").unwrap();
    p.set_approve(Some(&casefold(&py_collapse(&upvote.label)))); // the human's yes covers that one click
    assert!(p.click_candidate(&upvote).starts_with("clicked "));
}

fn hn_snapshot(items: &[(&str, &str, &str)]) -> eve_desktop_lanes::ax_candidates::Snapshot {
    let elements: Vec<Value> = items
        .iter()
        .enumerate()
        .map(|(i, (role, name, href))| {
            json!({"id": i.to_string(), "role": role, "name": name, "href": href, "x": 10, "y": 20 * i, "w": 40, "h": 16})
        })
        .collect();
    bl::snapshot_from_page(
        &json!({"elements": elements, "title": "Hacker News", "url": "https://news.ycombinator.com/"}),
        1,
        0.0,
    )
    .unwrap()
}

#[test]
fn test_browser_candidates_need_confirmation_by_label_and_by_href() {
    let s = hn_snapshot(&[
        ("link", "upvote", "https://news.ycombinator.com/vote?id=1&how=up"),
        ("link", "Show HN: a thing", "https://news.ycombinator.com/item?id=1"),
        ("link", "hide", "https://news.ycombinator.com/hide?id=1"),
        ("link", "", "https://news.ycombinator.com/vote?id=2&how=up"),
        ("link", "Saved articles", "https://news.ycombinator.com/favorites"),
        ("button", "Delete account", ""),
    ]);
    let asked: Vec<&str> =
        s.elements.iter().filter(|c| needs_confirmation(c, &s.href(&c.id))).map(|c| c.label.as_str()).collect();
    assert_eq!(asked, ["upvote", "hide", "", "Delete account"]);
}

#[test]
fn test_the_browser_lanes_snapshot_carries_query_bearing_hrefs_to_the_gate() {
    let raw = json!({"url": "https://news.ycombinator.com/", "title": "Hacker News", "elements": [
        {"id": 1, "role": "link", "name": "", "href": "https://news.ycombinator.com/vote?id=1&how=up&goto=news",
         "x": 5, "y": 40, "w": 10, "h": 10},
        {"id": 2, "role": "link", "name": "Show HN: a thing", "href": "https://example.com/thing",
         "x": 20, "y": 40, "w": 200, "h": 16},
        {"id": 3, "role": "button", "name": "Search", "x": 20, "y": 80, "w": 60, "h": 16}]});
    let snap = bl::snapshot_from_page(&raw, 1, 0.0).unwrap();
    assert_eq!(snap.href("1"), "https://news.ycombinator.com/vote?id=1&how=up&goto=news");
    assert_eq!(snap.href("3"), "");
    let vote = snap.elements.iter().find(|c| c.id == "1").unwrap();
    let title = snap.elements.iter().find(|c| c.id == "2").unwrap();
    assert!(needs_confirmation(vote, &snap.href(&vote.id)));
    assert!(!needs_confirmation(title, &snap.href(&title.id)));
}
