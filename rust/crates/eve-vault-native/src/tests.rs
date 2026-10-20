//! vault_native has no test file of its own: test_vault_mcp.py drives it through a fake with the same four
//! methods, and its live tests walk a real Chromium. These tests cover the walk over a fake tree (what the live
//! tests prove on a real one) and the `Field` / `Native` contract test_vault_mcp.py relies on.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::*;

type V = AxValue<usize>;

/// A tree of numbered nodes, each a map of attributes. Records every node an attribute was asked of, so a test
/// can prove a refused page was never read.
#[derive(Default)]
struct FakeTree {
    nodes: Vec<HashMap<&'static str, V>>,
    asked: RefCell<HashSet<usize>>,
}

impl FakeTree {
    fn add(&mut self, attrs: Vec<(&'static str, V)>) -> usize {
        self.nodes.push(attrs.into_iter().collect());
        self.nodes.len() - 1
    }
    fn set(&mut self, node: usize, name: &'static str, value: V) {
        self.nodes[node].insert(name, value);
    }
    fn kids(&mut self, node: usize, kids: &[usize]) {
        self.set(node, "AXChildren", V::Array(kids.iter().map(|&k| V::Element(k)).collect()));
    }
}

impl AxTree for FakeTree {
    type Node = usize;
    fn attr(&self, node: &usize, name: &str) -> Option<V> {
        self.asked.borrow_mut().insert(*node);
        self.nodes[*node].get(name).cloned()
    }
    fn element(&self, node: &usize) -> Element {
        Arc::new(*node)
    }
}

fn s(x: &str) -> V {
    V::Str(x.to_string())
}

fn web(tree: &mut FakeTree, url: &str) -> usize {
    tree.add(vec![("AXRole", s("AXWebArea")), ("AXURL", V::Url(url.to_string()))])
}

fn text(tree: &mut FakeTree, title: &str, secure: bool) -> usize {
    let mut attrs = vec![("AXRole", s("AXTextField")), ("AXTitle", s(title))];
    if secure {
        attrs.push(("AXSubrole", s(SECURE_SUBROLE)));
    }
    tree.add(attrs)
}

fn group(tree: &mut FakeTree) -> usize {
    tree.add(vec![("AXRole", s("AXGroup"))])
}

fn window(tree: &mut FakeTree, content: usize) -> usize {
    let w = tree.add(vec![("AXRole", s("AXWindow"))]);
    tree.kids(w, &[content]);
    w
}

fn app(tree: &mut FakeTree, windows: &[usize]) -> usize {
    let a = tree.add(vec![("AXRole", s("AXApplication"))]);
    tree.set(a, "AXWindows", V::Array(windows.iter().map(|&w| V::Element(w)).collect()));
    a
}

const LOGIN: &str = "https://secure.chase.com/login";
const EVIL: &str = "https://chase.com.evil.example/login";

#[test]
fn page_fields_reads_only_the_pages_want_accepts() {
    let mut t = FakeTree::default();
    let (user, pw) = (text(&mut t, "Email", false), text(&mut t, "Password", true));
    let form = group(&mut t);
    t.kids(form, &[user, pw]);
    let here = web(&mut t, LOGIN);
    t.kids(here, &[form]);
    let other_pw = text(&mut t, "Password", true);
    let there = web(&mut t, EVIL);
    t.kids(there, &[other_pw]);
    let (w1, w2) = (window(&mut t, here), window(&mut t, there));
    let a = app(&mut t, &[w1, w2]);
    let got = walk_app(&t, 7, "Google Chrome", &a, &|u| u == LOGIN, false);
    assert_eq!(got.iter().map(|f| f.words.as_str()).collect::<Vec<_>>(), ["Email", "Password"]);
    assert!(got.iter().all(|f| f.url == LOGIN && f.page == LOGIN && f.pid == 7 && f.app == "Google Chrome"));
    assert!(!got[0].secure() && got[1].secure());
    assert_eq!(got[1].subrole, SECURE_SUBROLE);
    assert_eq!(got[0].subrole, "");
    assert_eq!(got[0].role, "AXTextField");
    let asked = t.asked.borrow();
    assert!(asked.contains(&there), "the refused page's own URL is read to refuse it");
    assert!(!asked.contains(&other_pw), "a field on a refused page is never read");
}

#[test]
fn order_counts_nodes_top_to_bottom_and_the_last_window_is_walked_first() {
    let mut t = FakeTree::default();
    let (a1, a2) = (text(&mut t, "A1", false), text(&mut t, "A2", false));
    let page_a = web(&mut t, "https://a.example/");
    t.kids(page_a, &[a1, a2]);
    let b1 = text(&mut t, "B1", false);
    let page_b = web(&mut t, "https://b.example/");
    t.kids(page_b, &[b1]);
    let (wa, wb) = (window(&mut t, page_a), window(&mut t, page_b));
    let a = app(&mut t, &[wa, wb]);
    let got = walk_app(&t, 1, "Chrome", &a, &|_| true, false);
    // stack pops the last window first: wb(1) page_b(2) B1(3) wa(4) page_a(5) A1(6) A2(7)
    let seen: Vec<(&str, usize)> = got.iter().map(|f| (f.words.as_str(), f.order)).collect();
    assert_eq!(seen, [("B1", 3), ("A1", 6), ("A2", 7)]);
}

#[test]
fn a_text_field_outside_a_page_or_inside_a_field_is_not_listed() {
    let mut t = FakeTree::default();
    let address_bar = text(&mut t, "Address and search bar", false);
    let inner = text(&mut t, "inner", false);
    let combo = t.add(vec![("AXRole", s("AXComboBox")), ("AXTitle", s("Country"))]);
    t.kids(combo, &[inner]);
    let area = t.add(vec![("AXRole", s("AXTextArea")), ("AXPlaceholderValue", s("Message"))]);
    let page = web(&mut t, LOGIN);
    t.kids(page, &[combo, area]);
    let root = group(&mut t);
    t.kids(root, &[address_bar, page]);
    let w = window(&mut t, root);
    let a = app(&mut t, &[w]);
    let got = walk_app(&t, 1, "Chrome", &a, &|_| true, false);
    assert_eq!(got.iter().map(|f| (f.role.as_str(), f.words.as_str())).collect::<Vec<_>>(),
               [("AXComboBox", "Country"), ("AXTextArea", "Message")]);
    assert!(!t.asked.borrow().contains(&inner), "a field is not walked into");
}

#[test]
fn a_frame_needs_want_unless_frames_and_keeps_its_top_page() {
    let pay = "https://shop.example/checkout";
    let frame_url = "https://js.payments.example/frame";
    let build = || {
        let mut t = FakeTree::default();
        let email = text(&mut t, "Email", false);
        let cc = text(&mut t, "Card number", false);
        let frame = web(&mut t, frame_url);
        t.kids(frame, &[cc]);
        let page = web(&mut t, pay);
        t.kids(page, &[email, frame]);
        let w = window(&mut t, page);
        let a = app(&mut t, &[w]);
        (t, a)
    };
    let (t, a) = build();
    let plain = walk_app(&t, 1, "Chrome", &a, &|u| u == pay, false);
    assert_eq!(plain.iter().map(|f| f.words.as_str()).collect::<Vec<_>>(), ["Email"]);
    let (t, a) = build();
    let card = walk_app(&t, 1, "Chrome", &a, &|u| u == pay, true);
    assert_eq!(card.len(), 2);
    assert_eq!((card[1].words.as_str(), card[1].url.as_str(), card[1].page.as_str()), ("Card number", frame_url, pay));
    assert_eq!((card[0].url.as_str(), card[0].page.as_str()), (pay, pay));
    // a frame alone never stands for its page: with frames on, a refused top-level page is still skipped
    let (t, a) = build();
    assert!(walk_app(&t, 1, "Chrome", &a, &|u| u == frame_url, true).is_empty());
    // the frame accepted by want on its own, inside a refused page: the page is never entered
    let (t, a) = build();
    assert!(walk_app(&t, 1, "Chrome", &a, &|u| u == frame_url, false).is_empty());
}

#[test]
fn words_join_the_names_and_the_label_never_the_value() {
    let mut t = FakeTree::default();
    let label = t.add(vec![("AXRole", s("AXStaticText")), ("AXValue", s("Email address")), ("AXTitle", s("ignored"))]);
    let field = t.add(vec![
        ("AXRole", s("AXTextField")),
        ("AXTitle", s("")),
        ("AXDescription", s("  ")),
        ("AXPlaceholderValue", s("you@example.com")),
        ("AXDOMIdentifier", s("user")),
        ("AXHelp", V::Int(5)),
        ("AXValue", s("typed-secret")),
        ("AXTitleUIElement", V::Element(label)),
    ]);
    assert_eq!(words_of(&t, &field), "you@example.com user Email address");
    // the label's value is empty: its title stands in
    let label2 = t.add(vec![("AXValue", s("")), ("AXTitle", s("Password"))]);
    let field2 = t.add(vec![("AXTitleUIElement", V::Element(label2))]);
    assert_eq!(words_of(&t, &field2), "Password");
    // a label value that is not a string but truthy is used, and then dropped: the title is not asked
    let label3 = t.add(vec![("AXValue", V::Int(3)), ("AXTitle", s("Code"))]);
    let field3 = t.add(vec![("AXTitleUIElement", V::Element(label3))]);
    assert_eq!(words_of(&t, &field3), "");
    // Python's isspace counts U+001C..U+001F as blank
    let field4 = t.add(vec![("AXTitle", s("\u{1f}")), ("AXHelp", s("Help"))]);
    assert_eq!(words_of(&t, &field4), "Help");
}

#[test]
fn words_stop_at_200_characters() {
    let mut t = FakeTree::default();
    let long = "é".repeat(150);
    let field = t.add(vec![("AXTitle", s(&long)), ("AXDescription", s(&long))]);
    let got = words_of(&t, &field);
    assert_eq!(got.chars().count(), 200);
    assert!(got.starts_with(&long) && got.chars().nth(150) == Some(' '));
}

#[test]
fn focus_is_only_a_true_boolean() {
    let mut t = FakeTree::default();
    let yes = text(&mut t, "Yes", false);
    t.set(yes, "AXFocused", V::Bool(true));
    let one = text(&mut t, "One", false);
    t.set(one, "AXFocused", V::Int(1));
    let no = text(&mut t, "No", false);
    t.set(no, "AXFocused", V::Bool(false));
    let page = web(&mut t, LOGIN);
    t.kids(page, &[yes, one, no]);
    let w = window(&mut t, page);
    let a = app(&mut t, &[w]);
    let got = walk_app(&t, 1, "Chrome", &a, &|_| true, false);
    assert_eq!(got.iter().map(|f| f.focused).collect::<Vec<_>>(), [true, false, false]);
}

#[test]
fn the_url_is_the_absolute_string_or_str_of_what_the_browser_gives() {
    let mut t = FakeTree::default();
    let a = t.add(vec![("AXURL", V::Url(LOGIN.into()))]);
    let b = t.add(vec![("AXURL", s("about:blank"))]);
    let c = t.add(vec![]);
    let d = t.add(vec![("AXURL", s(""))]);
    let e = t.add(vec![("AXURL", V::Int(0))]);
    assert_eq!(url_of(&t, &a), LOGIN);
    assert_eq!(url_of(&t, &b), "about:blank");
    assert_eq!(url_of(&t, &c), "");
    assert_eq!(url_of(&t, &d), "");
    assert_eq!(url_of(&t, &e), "");
}

#[test]
fn a_page_with_no_url_has_no_fields_listed() {
    // url "" is falsy: a text field under it is not a page's field even when want accepts ""
    let mut t = FakeTree::default();
    let f = text(&mut t, "Search", false);
    let page = t.add(vec![("AXRole", s("AXWebArea"))]);
    t.kids(page, &[f]);
    let w = window(&mut t, page);
    let a = app(&mut t, &[w]);
    assert!(walk_app(&t, 1, "Chrome", &a, &|_| true, false).is_empty());
}

#[test]
fn a_walk_stops_at_max_nodes() {
    let mut t = FakeTree::default();
    let fields: Vec<usize> = (0..MAX_NODES + 10).map(|i| text(&mut t, &format!("f{i}"), false)).collect();
    let page = web(&mut t, LOGIN);
    t.kids(page, &fields);
    let w = window(&mut t, page);
    let a = app(&mut t, &[w]);
    let got = walk_app(&t, 1, "Chrome", &a, &|_| true, false);
    // window and page take two of the budget
    assert_eq!(got.len(), MAX_NODES - 2);
    assert_eq!(got.last().map(|f| f.order), Some(MAX_NODES));
}

#[test]
fn a_subrole_that_is_missing_or_empty_is_empty() {
    let mut t = FakeTree::default();
    let f = text(&mut t, "X", false);
    t.set(f, "AXSubrole", s(""));
    let g = text(&mut t, "Y", false);
    t.set(g, "AXSubrole", s("AXSearchField"));
    let page = web(&mut t, LOGIN);
    t.kids(page, &[f, g]);
    let w = window(&mut t, page);
    let a = app(&mut t, &[w]);
    let got = walk_app(&t, 1, "Chrome", &a, &|_| true, false);
    assert_eq!(got.iter().map(|f| f.subrole.as_str()).collect::<Vec<_>>(), ["", "AXSearchField"]);
}

#[test]
fn the_elements_are_the_walked_nodes() {
    let mut t = FakeTree::default();
    let f = text(&mut t, "X", false);
    let page = web(&mut t, LOGIN);
    t.kids(page, &[f]);
    let w = window(&mut t, page);
    let a = app(&mut t, &[w]);
    let got = walk_app(&t, 1, "Chrome", &a, &|_| true, false);
    let el = got[0].element.as_ref().and_then(|e| e.downcast_ref::<usize>()).copied();
    assert_eq!(el, Some(f));
}

#[test]
fn char_count_is_an_int_or_none() {
    assert_eq!(count_of::<usize>(Some(V::Int(14))), Some(14));
    assert_eq!(count_of::<usize>(Some(V::Bool(true))), Some(1));
    assert_eq!(count_of::<usize>(Some(V::Float(3.0))), None);
    assert_eq!(count_of::<usize>(Some(s("14"))), None);
    assert_eq!(count_of::<usize>(None), None);
}

#[test]
fn field_defaults_and_secure_match_the_dataclass() {
    let f = Field { pid: 1, app: "Google Chrome".into(), role: "AXTextField".into(), url: LOGIN.into(), ..Default::default() };
    assert_eq!((f.page.as_str(), f.words.as_str(), f.focused, f.order), ("", "", false, 0));
    assert!(f.element.is_none() && !f.secure());
    let pw = Field { subrole: "AXSecureTextField".into(), ..f.clone() };
    assert!(pw.secure());
    assert_eq!(f.clone(), f);
    assert_ne!(f, pw);
    let e: Element = Arc::new(3usize);
    let a = Field { element: Some(e.clone()), ..f.clone() };
    assert_eq!(a, Field { element: Some(e), ..f.clone() });
    assert_ne!(a, Field { element: Some(Arc::new(3usize)), ..f.clone() });
}

#[test]
fn the_constants_are_pythons() {
    let mut sorted = BROWSERS;
    sorted.sort();
    assert_eq!(sorted, BROWSERS, "walked in sorted order, as Python's sorted(BROWSERS)");
    assert_eq!(BROWSERS.len(), 12);
    assert!(BROWSERS.contains(&"com.google.chrome.for.testing") && BROWSERS.contains(&"company.thebrowser.Browser"));
    assert_eq!(TEXT_ROLES, ["AXTextField", "AXTextArea", "AXComboBox"]);
    assert_eq!(NAME_ATTRS, ["AXTitle", "AXDescription", "AXPlaceholderValue", "AXDOMIdentifier", "AXHelp"]);
    assert_eq!((SECURE_SUBROLE, MAX_NODES), ("AXSecureTextField", 40000));
}

/// test_vault_mcp.py's FakeMac: pages and their fields; page_fields walks only the pages `want` accepts.
struct FakeMac {
    fields: Vec<Field>,
    set: RefCell<Vec<(String, String)>>,
}

impl Native for FakeMac {
    fn page_fields(&self, want: &dyn Fn(&str) -> bool, frames: bool) -> Vec<Field> {
        self.fields.iter().filter(|f| want(&f.url) || (frames && !f.page.is_empty() && want(&f.page))).cloned().collect()
    }
    fn set_value(&self, field: &Field, text: &str) -> bool {
        self.set.borrow_mut().push((field.words.clone(), text.to_string()));
        true
    }
    fn char_count(&self, _field: &Field) -> Option<i64> {
        self.set.borrow().last().map(|(_, v)| v.chars().count() as i64).or(Some(0))
    }
}

#[test]
fn a_fake_with_the_four_methods_is_a_native_and_trusted_by_default() {
    let pw = Field { pid: 1, app: "Google Chrome".into(), role: "AXTextField".into(), subrole: SECURE_SUBROLE.into(),
                     url: LOGIN.into(), words: "Password".into(), order: 1, ..Default::default() };
    let mac = FakeMac { fields: vec![pw.clone()], set: RefCell::new(Vec::new()) };
    let native: Box<dyn Native> = Box::new(mac);
    assert!(native.trusted());
    let got = native.page_fields(&|u| u == LOGIN, false);
    assert_eq!(got, vec![pw]);
    assert!(native.page_fields(&|u| u == EVIL, false).is_empty());
    assert!(native.set_value(&got[0], "mcp-canary-pw!"));
    assert_eq!(native.char_count(&got[0]), Some(14));
}

#[cfg(not(target_os = "macos"))]
#[test]
fn native_mac_refuses_off_macos() {
    let err = NativeMac::new().err().expect("off macOS the constructor fails");
    assert_eq!(err.to_string(), "the vault fills fields only on macOS");
}

#[cfg(target_os = "macos")]
#[test]
fn native_mac_builds_on_macos_and_a_field_without_an_element_takes_nothing() {
    let mac = NativeMac::new().expect("macOS");
    let f = Field { url: LOGIN.into(), ..Default::default() };
    assert!(!mac.set_value(&f, "x"));
    assert_eq!(mac.char_count(&f), None);
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "live: walks the running browsers' Accessibility trees and asks each for AXManualAccessibility"]
fn live_native_mac_walks_running_browsers() {
    let mac = NativeMac::new().expect("macOS");
    // a want that refuses every page reads no field at all
    assert!(mac.page_fields(&|_| false, true).is_empty());
    if mac.trusted() {
        for f in mac.page_fields(&|u| u.starts_with("http"), false) {
            assert!(TEXT_ROLES.contains(&f.role.as_str()) && !f.url.is_empty() && f.element.is_some());
            assert_eq!(f, f.clone());
        }
    }
}
