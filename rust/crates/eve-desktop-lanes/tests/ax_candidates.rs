//! Port of bridge/tests/test_ax_candidates.py (hand-rolled raw walks, no Accessibility), plus the parts of
//! test_fixtures_ax.py, test_side_effect_links.py and test_side_effect_precision.py that exercise ax_candidates.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{json, Value};

use eve_desktop_lanes::ax_candidates::{
    self as ax, ax_snapshot, ax_walk_with, context_summary, describe, is_account_change, is_pressable, is_sensitive,
    needs_confirmation, objective_tokens, parse_qsl, pressable, rank_candidates, snapshot_from_dict, snapshot_from_raw,
    unquote, urlsplit, AxRef, AxScalar, AxWalkBackend, Candidate, Kinds, NodePath, Snapshot, DEFAULT_BUDGET_SECS,
    DEFAULT_MAX_NODES, SENSITIVE_LABEL,
};

/// ax_snapshot hands out a process-wide seq; tests that read it serialise here (Python ran them one at a time).
static SEQ_LOCK: Mutex<()> = Mutex::new(());

// ---- fakes -------------------------------------------------------------------------

#[derive(Clone)]
struct N {
    role: &'static str,
    title: String,
    frame: Option<Vec<f64>>,
    children: Vec<Value>,
    actions: Vec<&'static str>,
    subrole: &'static str,
    value: Value,
    description: &'static str,
    enabled: bool,
    focused: bool,
}

/// One raw node in the documented walk shape (ancestor_roles filled by `raw`).
fn node(role: &'static str, title: &str) -> N {
    N {
        role,
        title: title.to_string(),
        frame: Some(vec![0.0, 0.0, 10.0, 10.0]),
        children: Vec::new(),
        actions: Vec::new(),
        subrole: "",
        value: Value::Null,
        description: "",
        enabled: true,
        focused: false,
    }
}

impl N {
    fn frame(mut self, f: &[f64]) -> Self {
        self.frame = Some(f.to_vec());
        self
    }
    fn no_frame(mut self) -> Self {
        self.frame = None;
        self
    }
    fn kids(mut self, kids: Vec<Value>) -> Self {
        self.children = kids;
        self
    }
    fn press(mut self) -> Self {
        self.actions = vec!["AXPress"];
        self
    }
    fn subrole(mut self, s: &'static str) -> Self {
        self.subrole = s;
        self
    }
    fn value(mut self, v: Value) -> Self {
        self.value = v;
        self
    }
    fn description(mut self, d: &'static str) -> Self {
        self.description = d;
        self
    }
    fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }
    fn focused(mut self) -> Self {
        self.focused = true;
        self
    }
    fn v(self) -> Value {
        json!({"role": self.role, "subrole": self.subrole, "title": self.title, "description": self.description,
               "value": self.value, "frame": self.frame, "actions": self.actions, "enabled": self.enabled,
               "focused": self.focused, "children": self.children, "ancestor_roles": []})
    }
}

fn fill_ancestors(n: &mut Value, ancestors: &[String]) {
    n["ancestor_roles"] = json!(ancestors);
    let mut below = ancestors.to_vec();
    below.push(n["role"].as_str().unwrap_or_default().to_string());
    if let Some(children) = n["children"].as_array_mut() {
        for c in children {
            fill_ancestors(c, &below);
        }
    }
}

fn count(n: &Value) -> i64 {
    1 + n["children"].as_array().map_or(0, |c| c.iter().map(count).sum())
}

struct RawOpts {
    app: &'static str,
    subrole: &'static str,
    node_count: Option<i64>,
    truncated: bool,
    fullscreen: bool,
}

impl Default for RawOpts {
    fn default() -> Self {
        RawOpts { app: "Calendar", subrole: "AXStandardWindow", node_count: None, truncated: false, fullscreen: false }
    }
}

fn raw_with(children: Vec<Value>, o: &RawOpts) -> Value {
    let mut root = node("AXWindow", "Calendar").frame(&[0.0, 0.0, 900.0, 600.0]).subrole(o.subrole).kids(children).v();
    fill_ancestors(&mut root, &[]);
    let n = o.node_count.unwrap_or_else(|| count(&root));
    json!({"app": o.app, "bundle": "com.apple.iCal", "pid": 42, "title": root["title"], "fullscreen": o.fullscreen,
           "node_count": n, "truncated": o.truncated, "secs": 0.05, "root": root})
}

fn raw(children: Vec<Value>) -> Value {
    raw_with(children, &RawOpts::default())
}

fn snap_with(children: Vec<Value>, screen: Option<(i64, i64)>, o: &RawOpts) -> Snapshot {
    let r = raw_with(children, o);
    let _guard = SEQ_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    ax_snapshot(Some(42), DEFAULT_MAX_NODES, DEFAULT_BUDGET_SECS, Some(&|_, _, _| r.clone()), screen)
}

fn snap(children: Vec<Value>) -> Snapshot {
    snap_with(children, Some((1440, 900)), &RawOpts::default())
}

fn labels<'a>(cands: impl IntoIterator<Item = &'a Candidate>) -> Vec<String> {
    cands.into_iter().map(|c| c.label.clone()).collect()
}

fn rank(s: &Snapshot, objective: &str, max_out: i64) -> (Vec<Candidate>, usize) {
    rank_candidates(s, objective, max_out, Kinds::Pressable, false, false)
}

/// Calendar's view switcher and navigation, as the real walk shows them.
fn toolbar() -> Vec<Value> {
    vec![
        node("AXButton", "Add Event").frame(&[471.0, 98.0, 44.0, 52.0]).press().v(),
        node("AXRadioButton", "Day").frame(&[636.0, 106.0, 60.0, 36.0]).press().value(json!(0)).v(),
        node("AXRadioButton", "Week").frame(&[696.0, 106.0, 60.0, 36.0]).press().value(json!(0)).v(),
        node("AXRadioButton", "Month").frame(&[758.0, 106.0, 60.0, 36.0]).press().value(json!(1)).v(),
        node("AXRadioButton", "Year").frame(&[818.0, 106.0, 60.0, 36.0]).press().value(json!(0)).v(),
        node("AXButton", "previous month").frame(&[1086.0, 165.0, 27.0, 27.0]).press().v(),
        node("AXButton", "Today").frame(&[1115.0, 165.0, 68.0, 27.0]).press().v(),
        node("AXButton", "next month").frame(&[1185.0, 165.0, 27.0, 27.0]).press().v(),
        node("AXButton", "Delete Event").frame(&[300.0, 165.0, 60.0, 27.0]).press().v(),
        node("AXStaticText", "").value(json!("September 2026")).frame(&[300.0, 140.0, 200.0, 20.0]).v(),
    ]
}

fn by_label(s: &Snapshot) -> HashMap<String, Candidate> {
    s.elements.iter().map(|c| (c.label.clone(), c.clone())).collect()
}

// ---- the sensitive-label table -----------------------------------------------------------

fn long_label() -> String {
    "Keep every message from this sender in its current mailbox folder and ".to_string() + "archive the rest now"
}

const POSITIVES: &[&str] = &[
    "Delete Event",
    "Don't Save",
    "Don’t Save",
    "Replace",
    "OK",
    "Remove filter",
    "Export as PDF",
    "Send",
    "Pay now",
    "Sign in",
    "Log Out",
    "Move to Trash",
    "Close Window",
    "Share…",
    "Continue",
    "Apply",
    "Yes",
    "Empty Trash",
    "System Settings…",
    "Install now",
    "Discard changes",
    "Allow",
    "Upload",
    "Report",
    "Block sender",
    "Accept",
    "Agree",
    "Quit Calendar",
    "Restart",
    // 2026-09-21: every one of these read as harmless before the plan executor's audit
    "Place Order",
    "Place your order",
    "Order Now",
    "Confirm",
    "Book now",
    "Publish",
    "Transfer",
    "Clear History",
    "Cancel Subscription",
    "Merge pull request",
    "Format Disk",
    "Turn Off",
    "Turn On FileVault",
    "Approve",
    "Kill",
    "Add to Cart",
    "Mark as Spam",
    "Revert",
    "Sign Up",
    "Create Account",
];
const NEGATIVES: &[&str] = &[
    "Postcode",
    "Composer",
    "Dispatcher",
    "Week",
    "Accent color",
    "Reading List",
    "Sidebar",
    "Downloads",
    "Today",
    "Month",
    "Search Wikipedia",
    "Notes",
    "Calls",
    "Applications",
    "Blocked",
    "Replay",
    "Recents",
    "Sender",
    "Payments history",
    "Trashcan icon",
    "Continuous",
    "Bookmarks",
    "Books",
    "Orderly",
    "Sort Order",
    "Forward",
    "Format",
    "Formatting",
    "Transfers",
    "Confirmation",
    "Skills",
    "Turntable",
    "Publisher",
    "Merge All Windows",
];

#[test]
fn test_sensitive_label_table_positives() {
    let long = long_label();
    assert_eq!(long.chars().count(), 90);
    assert!(long.find("archive").unwrap() > 60);
    assert!(POSITIVES.len() >= 12);
    let wrong: Vec<&&str> = POSITIVES.iter().filter(|p| !is_sensitive(p)).collect();
    assert!(wrong.is_empty(), "{wrong:?}");
    assert!(is_sensitive(&long) && long.to_lowercase().find("archive").unwrap() > 60);
    assert!(SENSITIVE_LABEL.find("please delete this").unwrap().is_some());
    // Python's fail-closed branch for a label that is not text (None, 42) has no Rust caller: labels are &str.
}

#[test]
fn test_sensitive_label_table_negatives() {
    assert!(NEGATIVES.len() >= 12);
    let wrong: Vec<&&str> = NEGATIVES.iter().filter(|n| is_sensitive(n)).collect();
    assert!(wrong.is_empty(), "{wrong:?}");
    assert!(!is_sensitive("") && !is_sensitive("   "));
}

#[test]
fn test_sensitive_agrees_with_the_lookaround_pattern_and_survives_long_labels() {
    for label in POSITIVES.iter().chain(NEGATIVES).copied().chain(["okay", "ok?", "x-ok", "ok1", "Okay dokey"]) {
        assert_eq!(is_sensitive(label), SENSITIVE_LABEL.is_match(label.trim()).unwrap(), "{label}");
    }
    let long = "word ".repeat(4000) + "delete";
    assert!(is_sensitive(&long));
    assert!(!is_sensitive(&"word ".repeat(4000)));
}

#[test]
fn test_sensitive_phrases_match_across_whitespace_and_case() {
    assert!(is_sensitive("DON'T  SAVE") && is_sensitive("sign\tin") && is_sensitive("move  to   trash"));
    assert!(is_sensitive("check out") && is_sensitive("Checkout") && is_sensitive("shut down"));
}

// ---- derivation: labels, values, roles, tags ---------------------------------------------

#[test]
fn test_reading_order_ids_and_role_names() {
    let s = snap(vec![
        node("AXButton", "B").frame(&[50.0, 20.0, 10.0, 10.0]).v(),
        node("AXButton", "A").frame(&[10.0, 20.0, 10.0, 10.0]).v(),
        node("AXButton", "C").frame(&[0.0, 40.0, 10.0, 10.0]).v(),
        node("AXPopUpButton", "P").frame(&[0.0, 0.0, 10.0, 10.0]).v(),
    ]);
    assert_eq!(labels(&s.elements), ["P", "A", "B", "C"]);
    assert_eq!(s.elements.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["0", "1", "2", "3"]);
    assert!(s.elements[0].role == "pop up button" && s.elements[1].role == "button");
    assert!(s.app == "Calendar" && s.bundle == "com.apple.iCal" && s.title == "Calendar" && s.pid == 42);
    assert!(s.seq >= 1 && s.node_count == 5 && !s.truncated && s.secs == 0.05);
    assert_eq!(s.get("2").unwrap().label, "B");
    assert!(s.get("9").is_none());
}

#[test]
fn test_values_selected_checked_expanded_and_text() {
    let s = snap(vec![
        node("AXRadioButton", "Month").value(json!(1)).v(),
        node("AXRadioButton", "Week").value(json!(0)).frame(&[20.0, 0.0, 10.0, 10.0]).v(),
        node("AXCheckBox", "Bold").value(json!(1)).frame(&[40.0, 0.0, 10.0, 10.0]).v(),
        node("AXCheckBox", "Wide").value(json!(0)).frame(&[60.0, 0.0, 10.0, 10.0]).v(),
        node("AXCheckBox", "Dark").value(json!(1)).subrole("AXToggle").frame(&[80.0, 0.0, 10.0, 10.0]).v(),
        node("AXDisclosureTriangle", "More").value(json!(0)).frame(&[100.0, 0.0, 10.0, 10.0]).v(),
        node("AXTextField", "Name").value(json!("x".repeat(60))).frame(&[120.0, 0.0, 10.0, 10.0]).v(),
        node("AXStaticText", "").value(json!("A heading")).frame(&[140.0, 0.0, 10.0, 10.0]).v(),
        node("AXRadioButton", "Tab 1").subrole("AXTabButton").value(json!(1)).frame(&[160.0, 0.0, 10.0, 10.0]).v(),
    ]);
    let by = by_label(&s);
    assert!(by["Month"].value == "selected" && by["Week"].value.is_empty());
    assert!(by["Bold"].value == "checked" && by["Wide"].value == "unchecked");
    assert!(by["Dark"].value == "checked" && by["Dark"].role == "toggle");
    assert!(by["More"].value == "collapsed" && by["More"].role == "disclosure triangle");
    assert!(by["Name"].value == "x".repeat(39) + "…" && by["Name"].editable && by["Name"].role == "text field");
    assert!(by["A heading"].role == "static text" && by["A heading"].value.is_empty());
    assert!(by["Tab 1"].role == "tab" && by["Tab 1"].value == "selected");
}

#[test]
fn test_descendant_static_text_labels_rows_cells_and_links() {
    let s = snap(vec![
        node("AXRow", "")
            .frame(&[0.0, 0.0, 100.0, 20.0])
            .kids(vec![node("AXCell", "")
                .frame(&[0.0, 0.0, 100.0, 20.0])
                .kids(vec![node("AXStaticText", "").value(json!("Appearance")).frame(&[2.0, 2.0, 60.0, 16.0]).v()])
                .v()])
            .v(),
        node("AXLink", "")
            .frame(&[0.0, 30.0, 100.0, 20.0])
            .kids(vec![node("AXStaticText", "").value(json!("Go back")).frame(&[2.0, 32.0, 60.0, 16.0]).v()])
            .v(),
        node("AXCell", "")
            .frame(&[0.0, 60.0, 100.0, 20.0])
            .kids(vec![node("AXStaticText", "").value(json!("Storage")).frame(&[2.0, 62.0, 60.0, 16.0]).v()])
            .v(),
        node("AXButton", "")
            .frame(&[0.0, 90.0, 20.0, 20.0])
            .kids(vec![node("AXImage", "gear").frame(&[0.0, 90.0, 20.0, 20.0]).v()])
            .v(),
    ]);
    let by: Vec<(String, String)> = s.elements.iter().map(|c| (c.role.clone(), c.label.clone())).collect();
    let has = |r: &str, l: &str| by.iter().any(|(a, b)| a == r && b == l);
    assert!(has("row", "Appearance") && has("link", "Go back") && has("cell", "Storage"));
    assert!(has("button", "gear"));
    // the cell that only repeats its row's label is the same control: only the row is offered
    assert!(!has("cell", "Appearance"));
    assert!(has("static text", "Appearance")); // the text itself stays, for done_when checks
}

#[test]
fn test_description_is_a_label_and_untitled_buttons_are_not_candidates() {
    let s = snap(vec![
        node("AXButton", "").description("Reload this page").press().v(),
        node("AXButton", "").frame(&[20.0, 0.0, 16.0, 16.0]).press().v(), // a traffic light
        node("AXGroup", "").frame(&[40.0, 0.0, 16.0, 16.0]).v(),
    ]);
    assert_eq!(labels(&s.elements), ["Reload this page"]);
}

#[test]
fn test_untitled_editable_field_is_a_candidate_and_secure_is_tagged() {
    let s = snap(vec![
        node("AXTextField", "").frame(&[0.0, 0.0, 100.0, 20.0]).v(),
        node("AXTextField", "PIN").subrole("AXSecureTextField").frame(&[0.0, 30.0, 100.0, 20.0]).v(),
        node("AXSecureTextField", "Password").frame(&[0.0, 60.0, 100.0, 20.0]).v(),
        node("AXTextField", "Find").subrole("AXSearchField").frame(&[0.0, 90.0, 100.0, 20.0]).v(),
        node("AXTextArea", "Message").frame(&[0.0, 120.0, 100.0, 20.0]).v(),
        node("AXComboBox", "Font").frame(&[0.0, 150.0, 100.0, 20.0]).v(),
    ]);
    let by = by_label(&s);
    assert!(by[""].editable && by[""].role == "text field");
    assert!(by["PIN"].secure && by["Password"].secure && !by["Find"].secure);
    assert!(by["Find"].role == "search field" && by["Message"].role == "text area" && by["Font"].role == "combo box");
    assert!(["Find", "Message", "Font"].iter().all(|k| by[*k].editable));
}

#[test]
fn test_content_and_dialog_tags() {
    let s = snap(vec![
        node("AXToolbar", "").frame(&[0.0, 0.0, 900.0, 40.0]).kids(vec![node("AXButton", "New Tab").press().v()]).v(),
        node("AXScrollArea", "")
            .frame(&[0.0, 40.0, 900.0, 500.0])
            .kids(vec![node("AXWebArea", "")
                .frame(&[0.0, 40.0, 900.0, 500.0])
                .kids(vec![
                    node("AXLink", "Search").frame(&[10.0, 50.0, 50.0, 20.0]).v(),
                    node("AXButton", "Search page").frame(&[10.0, 80.0, 50.0, 20.0]).press().v(),
                ])
                .v()])
            .v(),
        node("AXSheet", "")
            .frame(&[100.0, 100.0, 400.0, 200.0])
            .kids(vec![
                node("AXStaticText", "")
                    .value(json!("Do you want to save the changes?"))
                    .frame(&[110.0, 110.0, 300.0, 20.0])
                    .v(),
                node("AXButton", "Don't Save").frame(&[110.0, 250.0, 80.0, 20.0]).press().v(),
                node("AXButton", "Cancel").frame(&[200.0, 250.0, 80.0, 20.0]).press().v(),
            ])
            .v(),
    ]);
    let by = by_label(&s);
    assert!(!by["New Tab"].in_content && by["Search"].in_content && by["Search page"].in_content);
    assert!(by["Don't Save"].in_dialog && by["Cancel"].in_dialog && !by["New Tab"].in_dialog);
    assert_eq!(s.dialog_text, "Do you want to save the changes?");
}

#[test]
fn test_dialog_window_tags_everything_and_caps_the_text() {
    let long = "x".repeat(120);
    let s = snap_with(
        vec![
            node("AXStaticText", "").value(json!(long)).frame(&[0.0, 0.0, 300.0, 20.0]).v(),
            node("AXButton", "OK").frame(&[0.0, 30.0, 40.0, 20.0]).v(),
        ],
        Some((1440, 900)),
        &RawOpts { subrole: "AXDialog", ..RawOpts::default() },
    );
    assert!(s.elements.iter().all(|c| c.in_dialog) && s.dialog_text == "x".repeat(80));
    assert!(rank(&s, "press ok", 5).0.is_empty());
}

#[test]
fn test_focused_candidate_and_context_lines() {
    let mut kids = vec![
        node("AXStaticText", "").value(json!("September 2026")).frame(&[300.0, 140.0, 200.0, 20.0]).v(),
        node("AXStaticText", "").value(json!("Calendar")).frame(&[0.0, 0.0, 50.0, 10.0]).v(), // equals the title: skipped
        node("AXStaticText", "").value(json!("September 2026")).frame(&[0.0, 300.0, 50.0, 10.0]).v(), // duplicate
        node("AXStaticText", "").value(json!("y".repeat(61))).frame(&[0.0, 320.0, 50.0, 10.0]).v(), // too long
        node("AXRadioButton", "Month").value(json!(1)).focused().frame(&[758.0, 106.0, 60.0, 36.0]).v(),
    ];
    for i in 0..9 {
        kids.push(
            node("AXStaticText", "")
                .value(json!(format!("Event {i}")))
                .frame(&[0.0, 400.0 + f64::from(i) * 10.0, 50.0, 10.0])
                .v(),
        );
    }
    let s = snap(kids);
    assert_eq!(s.focused.as_ref().unwrap().label, "Month");
    assert_eq!(s.context_lines[..2], ["title: Calendar", "focused: radio button Month"]);
    assert_eq!(s.context_lines[2], "September 2026");
    assert!(s.context_lines.len() <= 8 && !s.context_lines[2..].contains(&"Calendar".to_string()));
    assert!(!s.context_lines.contains(&"y".repeat(61)));
}

#[test]
fn test_viewport_filter_and_bad_frames() {
    let s = snap_with(
        vec![
            node("AXButton", "On").frame(&[890.0, 590.0, 20.0, 20.0]).press().v(), // overlaps the corner
            node("AXButton", "Right").frame(&[900.0, 10.0, 20.0, 20.0]).press().v(),
            node("AXButton", "Below").frame(&[10.0, 600.0, 20.0, 20.0]).press().v(),
            node("AXButton", "Left").frame(&[-30.0, 10.0, 20.0, 20.0]).press().v(),
            node("AXButton", "Above").frame(&[10.0, -20.0, 20.0, 20.0]).press().v(),
            node("AXButton", "Flat").frame(&[10.0, 10.0, 0.0, 20.0]).press().v(),
            node("AXButton", "Nowhere").no_frame().press().v(),
            node("AXButton", "Odd").frame(&[1.0, 2.0, 3.0]).press().v(),
        ],
        Some((900, 600)),
        &RawOpts::default(),
    );
    assert_eq!(labels(&s.elements), ["On"]);
    // no screen given: no viewport filter, only the frame sanity checks
    let s = snap_with(
        vec![
            node("AXButton", "Right").frame(&[900.0, 10.0, 20.0, 20.0]).v(),
            node("AXButton", "Flat").frame(&[10.0, 10.0, 0.0, 20.0]).v(),
        ],
        None,
        &RawOpts::default(),
    );
    assert_eq!(labels(&s.elements), ["Right"]);
}

#[test]
fn test_no_window_snapshot() {
    let r = json!({"app": "Slack", "bundle": "com.tinyspeck.slackmacgap", "pid": 7, "title": "", "fullscreen": false,
                   "node_count": 0, "truncated": false, "secs": 0.001, "root": null});
    let s = ax_snapshot(Some(7), DEFAULT_MAX_NODES, DEFAULT_BUDGET_SECS, Some(&|_, _, _| r.clone()), None);
    assert!(s.elements.is_empty() && !s.truncated && s.node_count == 0 && s.app == "Slack");
    assert!(s.focused.is_none() && s.dialog_text.is_empty() && s.context_lines.is_empty());
    assert_eq!(rank(&s, "anything", 5), (vec![], 0));
}

#[test]
fn test_truncation_flags_pass_through_and_disabled_is_kept_but_tagged() {
    let s = snap_with(
        vec![node("AXButton", "Go").disabled().press().v()],
        Some((1440, 900)),
        &RawOpts { truncated: true, node_count: Some(800), ..RawOpts::default() },
    );
    assert!(s.truncated && s.node_count == 800);
    assert!(!s.elements[0].enabled);
    assert!(rank(&s, "go", 5).0.is_empty());
}

#[test]
fn test_ax_snapshot_passes_the_budget_to_the_walk_and_counts_seq() {
    let seen: Mutex<Vec<(i64, usize, f64)>> = Mutex::new(Vec::new());
    let walk = |pid: i64, max_nodes: usize, budget: f64| {
        seen.lock().unwrap().push((pid, max_nodes, budget));
        raw(vec![])
    };
    let _guard = SEQ_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let a = ax_snapshot(Some(5), 800, 0.2, Some(&walk), None);
    let b = ax_snapshot(Some(5), DEFAULT_MAX_NODES, DEFAULT_BUDGET_SECS, Some(&walk), None);
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0], (5, 800, 0.2));
    assert_eq!(seen[1], (5, 4000, 0.5));
    assert_eq!(b.seq, a.seq + 1);
}

#[test]
fn test_fullscreen_flag_and_skipped_scrollbar_subtrees() {
    let s = snap_with(
        vec![
            node("AXScrollBar", "").frame(&[0.0, 0.0, 10.0, 100.0]).kids(vec![node("AXButton", "Up").press().v()]).v(),
            node("AXButton", "Ok fine").frame(&[20.0, 0.0, 10.0, 10.0]).press().v(),
        ],
        Some((1440, 900)),
        &RawOpts { fullscreen: true, ..RawOpts::default() },
    );
    assert!(s.fullscreen);
    assert_eq!(labels(&s.elements), ["Ok fine"]);
}

// ---- the walker's budget (fake clock, fake AX) -------------------------------------------

/// A fake Accessibility: element 0 is the window, 1..=50 its static-text leaves (Python's fake
/// ApplicationServices module). `frames` gives the leaves a position and size.
struct FakeAx {
    frames: bool,
}

impl AxWalkBackend for FakeAx {
    type Element = usize;

    fn app_identity(&self, _pid: i64) -> (String, String) {
        ("Fake".into(), "com.fake".into())
    }

    fn window(&self, _pid: i64) -> Option<usize> {
        Some(0)
    }

    fn fullscreen(&self, _window: &usize) -> bool {
        false
    }

    fn values(&self, el: &usize) -> Option<Vec<AxScalar<usize>>> {
        let window = *el == 0;
        let name = if window { "window".to_string() } else { format!("leaf{}", el - 1) };
        let (pos, size) = if self.frames && !window {
            (AxScalar::Point(10.0, 20.0 * *el as f64), AxScalar::Size(40.0, 16.0))
        } else {
            (AxScalar::Null, AxScalar::Null)
        };
        Some(vec![
            AxScalar::Str(if window { "AXWindow" } else { "AXStaticText" }.into()),
            AxScalar::Str(String::new()),
            AxScalar::Str(name),
            AxScalar::Str(String::new()),
            AxScalar::Null,
            pos,
            size,
            AxScalar::Bool(true),
            AxScalar::Bool(false),
            AxScalar::List(if window { (1..=50).collect() } else { Vec::new() }), // static texts: ask for actions
        ])
    }

    fn action_names(&self, _el: &usize) -> Option<Vec<String>> {
        Some(vec!["AXPress".into()])
    }

    fn element_ref(&self, el: &usize) -> AxRef {
        AxRef::new(*el)
    }
}

#[test]
fn test_walk_truncates_at_max_nodes_and_at_budget() {
    let cursor = Mutex::new(0.0f64);
    let clock = || {
        let mut t = cursor.lock().unwrap();
        *t += 0.01;
        *t
    };
    let fake = FakeAx { frames: false };
    let r = ax_walk_with(&fake, 1, 10, 100.0, true, &clock, None);
    assert!(r["truncated"] == json!(true) && r["node_count"] == json!(10));
    assert_eq!(r["root"]["children"].as_array().unwrap().len(), 9);
    assert!(r["app"] == "Fake" && r["title"] == "window" && r["root"]["ancestor_roles"] == json!([]));
    assert_eq!(r["root"]["children"][0]["ancestor_roles"], json!(["AXWindow"]));
    *cursor.lock().unwrap() = 0.0;
    let r = ax_walk_with(&fake, 1, 1000, 0.15, true, &clock, None);
    let n = r["node_count"].as_i64().unwrap();
    assert!(r["truncated"] == json!(true) && 1 < n && n < 51 && r["secs"].as_f64().unwrap() > 0.15);
    *cursor.lock().unwrap() = 0.0;
    let r = ax_walk_with(&fake, 1, 1000, 100.0, true, &clock, None);
    assert!(r["truncated"] == json!(false) && r["node_count"] == json!(51));
    assert!(serde_json::to_string(&r).is_ok()); // JSON-serialisable
    assert_eq!(r["root"]["children"][0]["actions"], json!(["AXPress"]));
    assert!(snapshot_from_raw(&r, 0, None, None).elements.is_empty()); // the fake has no frames: nothing to click
}

#[test]
fn test_walk_keeps_each_candidates_live_element() {
    let fake = FakeAx { frames: true };
    let mut refs: HashMap<NodePath, AxRef> = HashMap::new();
    let clock = || 0.0;
    let r = ax_walk_with(&fake, 1, 1000, 100.0, true, &clock, Some(&mut refs));
    assert_eq!(refs.len(), 51);
    assert_eq!(refs[&vec![]].downcast_ref::<usize>(), Some(&0));
    assert_eq!(refs[&vec![4]].downcast_ref::<usize>(), Some(&5));
    let s = snapshot_from_raw(&r, 1, None, Some(&refs));
    assert_eq!(s.elements.len(), 50);
    for c in &s.elements {
        let el = s.element_ref(&c.id).and_then(|r| r.downcast_ref::<usize>()).copied().unwrap();
        assert_eq!(c.label, format!("leaf{}", el - 1));
    }
    // static texts carry their (empty) readings: no value means no readout
    assert!(s.readouts.is_empty());
    assert!(snapshot_from_raw(&r, 1, None, None).refs.is_empty());
}

#[test]
fn test_walk_without_a_pid_or_a_window_is_empty() {
    let clock = || 0.0;
    let r = ax_walk_with(&FakeAx { frames: false }, 0, 10, 1.0, true, &clock, None);
    assert_eq!(r["root"], Value::Null);
    assert_eq!(r["node_count"], json!(0));
    let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["app", "bundle", "pid", "title", "fullscreen", "node_count", "truncated", "secs", "root"]);
}

// ---- rank_candidates: drops, scoring, cap ----------------------------------------------

#[test]
fn test_rank_drops_disabled_unlabelled_dialog_secure_page_links_and_sensitive() {
    let s = snap(vec![
        node("AXButton", "Disabled").disabled().press().v(),
        node("AXButton", "").frame(&[20.0, 0.0, 10.0, 10.0]).press().v(),
        node("AXSheet", "").frame(&[100.0, 100.0, 400.0, 200.0]).kids(vec![node("AXButton", "Cancel").press().v()]).v(),
        node("AXTextField", "Password").subrole("AXSecureTextField").frame(&[0.0, 30.0, 100.0, 20.0]).v(),
        node("AXWebArea", "")
            .frame(&[0.0, 60.0, 900.0, 400.0])
            .kids(vec![
                node("AXLink", "Search").frame(&[10.0, 70.0, 50.0, 20.0]).v(),
                node("AXImage", "Logo").frame(&[10.0, 100.0, 50.0, 20.0]).press().v(),
                node("AXButton", "Play").frame(&[10.0, 130.0, 50.0, 20.0]).press().v(),
            ])
            .v(),
        node("AXButton", "Delete Event").frame(&[0.0, 500.0, 60.0, 20.0]).press().v(),
        node("AXButton", "Week").frame(&[0.0, 530.0, 60.0, 20.0]).press().v(),
    ]);
    let (kept, dropped) = rank(&s, "", 20);
    assert_eq!(labels(&kept), ["Play", "Week"]);
    assert_eq!(dropped, 0);
    let (kept, _) = rank_candidates(&s, "", 20, Kinds::Pressable, true, false);
    assert_eq!(labels(&kept), ["Play", "Week", "Search", "Logo"]);
    let (kept, _) = rank_candidates(&s, "", 20, Kinds::Pressable, false, true);
    assert_eq!(labels(&kept), ["Play", "Delete Event", "Week"]); // reading order on equal scores
    let (kept, _) = rank_candidates(&s, "", 20, Kinds::Editable, false, false);
    assert!(kept.is_empty()); // the only field is secure
                              // Python's ValueError for kinds="anything" cannot be written: Kinds is an enum.
}

#[test]
fn test_rank_scores_objective_tokens_over_role_prior_and_keeps_reading_order_on_ties() {
    let s = snap(toolbar());
    let (kept, dropped) = rank(&s, "switch to week view", 12);
    assert_eq!(labels(&kept)[0], "Week");
    // ties keep reading order (y, then x): the buttons and radio buttons (prior 3) in their rows
    assert_eq!(labels(&kept)[1..], ["Add Event", "Day", "Month", "Year", "previous month", "Today", "next month"]);
    assert!(!labels(&kept).contains(&"Delete Event".to_string()) && dropped == 0);
    assert!(!labels(&kept).contains(&"September 2026".to_string())); // static text without AXPress is not pressable
                                                                     // a distractor that shares a token with the goal ranks with the target: the model, not the code, decides
    let (kept, _) = rank(&s, "go back one month", 3);
    assert_eq!(labels(&kept), ["Month", "previous month", "next month"]); // all +4 for "month", reading order
    assert_eq!(describe(&kept[1], "click", 40), "click button: previous month");
}

#[test]
fn test_rank_matches_tokens_in_values_and_caps_with_a_count() {
    let s = snap(toolbar());
    let (kept, _) = rank(&s, "the selected view", 12);
    assert_eq!(labels(&kept)[0], "Month"); // "selected" is in its value
    let (kept, dropped) = rank(&s, "", 3);
    assert!(kept.len() == 3 && dropped == 5);
    let (kept, dropped) = rank(&s, "", 0);
    assert!(kept.is_empty() && dropped == 8);
    let (kept, dropped) = rank(&s, "", -4);
    assert!(kept.is_empty() && dropped == 8);
}

#[test]
fn test_objective_tokens_drop_stop_words_and_short_words() {
    assert_eq!(objective_tokens("Switch to the Week view"), ["week"]);
    assert_eq!(objective_tokens("go back a page, please"), ["back", "page"]);
    assert_eq!(objective_tokens("show the whole month"), ["whole", "month"]);
    assert_eq!(objective_tokens("add a new event"), ["add", "new", "event"]);
    assert_eq!(objective_tokens("month month MONTH"), ["month"]);
}

#[test]
fn test_pressable_by_action_or_role() {
    let s = snap(vec![
        node("AXStaticText", "").value(json!("Lunch")).press().v(),
        node("AXStaticText", "").value(json!("Header")).frame(&[0.0, 20.0, 10.0, 10.0]).v(),
        node("AXRow", "Wi-Fi").frame(&[0.0, 40.0, 10.0, 10.0]).v(),
        node("AXSlider", "Volume").frame(&[0.0, 60.0, 10.0, 10.0]).v(),
        node("AXImage", "Cover").frame(&[0.0, 80.0, 10.0, 10.0]).v(),
    ]);
    let by = by_label(&s);
    assert!(is_pressable(&by["Lunch"]) && !is_pressable(&by["Header"]) && is_pressable(&by["Wi-Fi"]));
    assert!(!is_pressable(&by["Volume"]) && !is_pressable(&by["Cover"]));
    assert_eq!(labels(pressable(&s)), ["Lunch", "Wi-Fi"]);
}

// ---- describe / context_summary --------------------------------------------------------

fn cand(role: &str, label: &str, value: &str) -> Candidate {
    Candidate {
        id: "1".into(),
        role: role.into(),
        label: label.into(),
        value: value.into(),
        frame: (1, 2, 3, 4),
        actions: vec!["AXPress".into()],
        enabled: true,
        app: "Calendar".into(),
        ..Candidate::default()
    }
}

#[test]
fn test_describe_strips_urls_collapses_space_and_caps() {
    assert_eq!(describe(&cand("radio button", "Week", "selected"), "click", 40), "click radio button: Week (selected)");
    assert_eq!(
        describe(&cand("link", "Visit  https://en.wikipedia.org/wiki/Foo  now", ""), "click", 40),
        "click link: Visit now"
    );
    assert_eq!(describe(&cand("link", "www.example.com", ""), "click", 40), "click link");
    let long = "Add AppleCare Coverage, There are 52 days left to protect your AirPods against accidental damage.";
    let d = describe(&cand("row", long, ""), "click", 40);
    assert_eq!(d, "click row: Add AppleCare Coverage, There are 52 da…");
    assert!(d.chars().count() <= 11 + 40);
    assert_eq!(
        describe(&cand("text field", "Search", &"x".repeat(40)), "type into", 40),
        format!("type into text field: Search ({}…)", "x".repeat(23))
    );
}

#[test]
fn test_context_summary_lists_title_focused_dialog_and_texts_capped() {
    let s = snap(vec![
        node("AXStaticText", "").value(json!("September 2026")).frame(&[300.0, 140.0, 200.0, 20.0]).v(),
        node("AXRadioButton", "Month").value(json!(1)).focused().frame(&[758.0, 106.0, 60.0, 36.0]).v(),
        node("AXStaticText", "").value(json!("Labor Day")).frame(&[0.0, 300.0, 50.0, 10.0]).v(),
    ]);
    assert_eq!(
        context_summary(&s, 300),
        "app: Calendar; title: Calendar; focused: radio button Month; text: September 2026 | Labor Day"
    );
    assert_eq!(context_summary(&s, 30), "app: Calendar; title: Calenda…");
    let d = snap(vec![node("AXSheet", "")
        .frame(&[0.0, 0.0, 400.0, 200.0])
        .kids(vec![node("AXStaticText", "").value(json!("Save changes?")).frame(&[10.0, 10.0, 100.0, 20.0]).v()])
        .v()]);
    assert!(context_summary(&d, 300).contains("dialog: Save changes?"));
}

// ---- dict round trip and raw re-derivation -----------------------------------------------

#[test]
fn test_to_dict_from_dict_round_trip_is_json_safe() {
    let mut kids = toolbar();
    kids.push(node("AXRadioButton", "Month").value(json!(1)).focused().frame(&[0.0, 0.0, 10.0, 10.0]).v());
    let s = snap(kids);
    let d: Value = serde_json::from_str(&serde_json::to_string(&s.to_dict()).unwrap()).unwrap();
    let back = snapshot_from_dict(&d).unwrap();
    assert!(back == s && back.focused == s.focused && back.elements[3].frame == s.elements[3].frame);
}

#[test]
fn test_snapshot_from_raw_is_deterministic_and_matches_ax_snapshot() {
    let mut r = raw(toolbar());
    let a = snapshot_from_raw(&r, 3, Some((900, 600)), None);
    let b = snapshot_from_raw(&serde_json::from_str(&r.to_string()).unwrap(), 3, Some((900, 600)), None);
    assert!(a == b && a.seq == 3);
    let c = ax_snapshot(Some(42), DEFAULT_MAX_NODES, DEFAULT_BUDGET_SECS, Some(&|_, _, _| r.clone()), Some((900, 600)));
    assert!(c.elements == a.elements && c.context_lines == a.context_lines && c.focused == a.focused);
    // the raw dump's own title wins over the root's when both are present
    r["title"] = json!("Other");
    assert_eq!(snapshot_from_raw(&r, 0, None, None).title, "Other");
}

#[test]
fn test_candidate_from_dict_tolerates_missing_optional_fields() {
    let c = Candidate::from_dict(&json!({"id": 3, "role": "button", "frame": [1, 2, 3, 4]})).unwrap();
    assert!(c.id == "3" && c.label.is_empty() && c.actions.is_empty() && c.enabled && !c.secure);
    assert_eq!(c.frame, (1, 2, 3, 4));
    // the required keys are required (Python's KeyError)
    assert!(Candidate::from_dict(&json!({"role": "button", "frame": [1, 2, 3, 4]})).is_err());
    assert!(Candidate::from_dict(&json!({"id": 1, "role": "button"})).is_err());
}

// ---- test_fixtures_ax.py: every committed fixture re-derives from its raw walk --------------

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../bridge/tests/fixtures/ax")
}

fn fixture_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    for set in ["select", "holdout"] {
        if let Ok(entries) = std::fs::read_dir(fixture_root().join(set)) {
            for e in entries.flatten() {
                if e.path().extension().is_some_and(|x| x == "json") {
                    files.push(e.path());
                }
            }
        }
    }
    files.sort();
    files
}

/// Python's json.loads reads the bare NaN / Infinity / -Infinity the walker's dumps carry (an AXScrollBar's value,
/// a System Settings row's frame); serde_json does not. Each becomes null, which every reader treats as Python
/// treats those floats here: an infinite frame is no frame, and no NaN sits on a candidate.
fn load(path: &Path) -> Value {
    let text = std::fs::read_to_string(path).unwrap();
    let mut out = String::with_capacity(text.len());
    let (mut in_str, mut escaped) = (false, false);
    let mut rest = text.as_str();
    while let Some(c) = rest.chars().next() {
        if in_str {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
        } else if c == '"' {
            in_str = true;
        } else if let Some(tok) = ["-Infinity", "Infinity", "NaN"].iter().find(|t| rest.starts_with(**t)) {
            out.push_str("null");
            rest = &rest[tok.len()..];
            continue;
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    serde_json::from_str(&out).unwrap()
}

/// Python's `==` over decoded JSON: numbers by value (1 == 1.0, True == 1), dicts regardless of key order.
fn py_eq(a: &Value, b: &Value) -> bool {
    let num = |v: &Value| match v {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    };
    match (a, b) {
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| py_eq(p, q)),
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| py_eq(v, w)))
        }
        (Value::Number(_) | Value::Bool(_), Value::Number(_) | Value::Bool(_)) => num(a) == num(b),
        _ => a == b,
    }
}

#[test]
fn test_fixtures_exist() {
    assert!(!fixture_files().is_empty(), "no fixtures under {}", fixture_root().display());
}

#[test]
fn test_no_fixture_is_in_both_sets() {
    let names: Vec<String> =
        fixture_files().iter().map(|p| p.file_stem().unwrap().to_string_lossy().into_owned()).collect();
    let mut unique = names.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(names.len(), unique.len());
}

#[test]
fn test_fixture_rederives_from_its_raw_dump() {
    for path in fixture_files() {
        let data = load(&path);
        let captured = &data["captured"];
        assert!(captured["truncated"] == json!(false) && data["raw"]["truncated"] == json!(false), "{path:?}");
        assert_eq!(captured["redacted"], json!(true), "fixtures carry synthetic labels only (README)");
        assert_eq!(captured["ax_candidates_sha"].as_str().unwrap().len(), 64);
        let stored = snapshot_from_dict(&data["snapshot"]).unwrap();
        let screen = data["screen"]
            .as_array()
            .filter(|s| !s.is_empty())
            .map(|s| (s[0].as_i64().unwrap(), s[1].as_i64().unwrap()));
        let derived = snapshot_from_raw(&data["raw"], stored.seq, screen, None);
        assert!(
            py_eq(&derived.to_dict(), &data["snapshot"]),
            "{path:?}: the snapshot no longer matches its raw walk: re-derive it"
        );
        assert!(!derived.truncated && !derived.elements.is_empty(), "a fixture is a complete, non-empty snapshot");
        // and the stored snapshot reads back as the same Snapshot
        assert!(derived == stored, "{path:?}");
    }
}

#[test]
fn test_fixture_cases_point_at_real_candidates() {
    for path in fixture_files() {
        let data = load(&path);
        let snap = snapshot_from_dict(&data["snapshot"]).unwrap();
        let cases = data["cases"].as_array().unwrap();
        assert!(!cases.is_empty(), "a fixture without cases measures nothing");
        let key = |v: &Value| match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        for case in cases {
            assert!(case["goal"].as_str().is_some_and(|g| !g.trim().is_empty()));
            assert!(case["expected"] == "click" || case["expected"] == "abstain");
            assert!(case["overlap"].is_boolean() && case["distractor"].is_boolean());
            if case["expected"] == "abstain" {
                assert!(case.get("expected_id").is_none_or(Value::is_null));
            } else {
                let c = snap.get(&key(&case["expected_id"]));
                assert!(c.is_some(), "{path:?}: {} expects id {}", case["goal"], case["expected_id"]);
                assert!(!c.unwrap().label.is_empty(), "the expected control must be labelled");
            }
            for forbidden in case["must_not_offer"].as_array().into_iter().flatten() {
                assert!(snap.get(&key(forbidden)).is_some(), "must_not_offer id {forbidden} is not in the snapshot");
            }
        }
    }
}

// redaction denylist (review 2026-09-21): a fixture may not carry the owner's data

const EMAIL: &str = r"[\w.+-]+@(?!example\.)[\w-]+\.[a-z]{2,}";

/// The e-mail pattern's negative lookahead, without a backtracking engine (fancy-regex gives up on the 4.7 MB
/// Safari fixture, where Python's re does not): some `@` with a local-part character before it, not followed by
/// `example.`, and followed by a domain.
fn email_found(text: &str) -> bool {
    let local = regex::Regex::new(r"^[\w.+-]$").unwrap();
    let example = regex::Regex::new(r"(?i)^example\.").unwrap();
    let domain = regex::Regex::new(r"(?i)^[\w-]+\.[a-z]{2,}").unwrap();
    text.match_indices('@').any(|(at, _)| {
        let before = text[..at].chars().next_back().is_some_and(|c| local.is_match(c.encode_utf8(&mut [0; 4])));
        let after = &text[at + 1..];
        before && !example.is_match(after) && domain.is_match(after)
    })
}

fn denied(text: &str) -> Vec<String> {
    let mut hits: Vec<String> = [
        r"gurucharan",
        r"lingamallu",
        r"\+?1?\s?\(?\d{3}\)?[\s.-]\d{3}[\s.-]\d{4}",
        r"era\.world",
        r"claude-501",
        r"CodePath",
        r"storeybox",
    ]
    .iter()
    .filter(|p| regex::Regex::new(&format!("(?i){p}")).unwrap().is_match(text))
    .map(|p| p.to_string())
    .collect();
    if email_found(text) {
        hits.push(EMAIL.to_string());
    }
    hits
}

#[test]
fn test_denylist_catches_a_known_positive_control() {
    let bad = "row \"Gurucharan Lingamallu, Apple Account\" field \"guru.x@gmail.com\" text \"+1 (425) 555-0100\"";
    let hits = denied(bad);
    assert!(hits.iter().any(|h| h.contains("gurucharan")));
    assert!(hits.iter().any(|h| h.contains('@')));
    assert!(hits.iter().any(|h| h.contains("d{3}")));
    assert!(denied("row \"Sam Rivera, Apple Account\" field \"sam@example.org\" text \"phone redacted\"").is_empty());
}

#[test]
fn test_no_fixture_carries_owner_data() {
    let files = fixture_files();
    assert!(!files.is_empty(), "no fixtures");
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(denied(&text).is_empty(), "{f:?}: {:?}", denied(&text));
    }
}

// ---- test_side_effect_links.py: account-changing page controls ------------------------------------

/// A browser snapshot from (role, name, href) rows, the shape browser_lane.snapshot_from_page builds (app
/// "browser", every control AXPress, in_content false, the raw hrefs carried).
fn page(items: &[(&str, &str, &str)]) -> Snapshot {
    let mut elements: Vec<Candidate> = items
        .iter()
        .enumerate()
        .map(|(i, (role, name, _))| Candidate {
            id: i.to_string(),
            role: (*role).into(),
            label: (*name).into(),
            frame: (10, 20 * i as i64, 40, 16),
            actions: vec!["AXPress".into()],
            enabled: true,
            app: "browser".into(),
            ..Candidate::default()
        })
        .collect();
    elements.sort_by_key(|c| (c.frame.1 / 8, c.frame.0));
    Snapshot {
        seq: 1,
        app: "browser".into(),
        bundle: "eve.browser".into(),
        title: "Hacker News".into(),
        pid: 1,
        node_count: elements.len() as i64,
        elements,
        context_lines: vec!["title: Hacker News".into(), "url: https://news.ycombinator.com/".into()],
        hrefs: items
            .iter()
            .enumerate()
            .filter(|(_, (_, _, href))| !href.is_empty())
            .map(|(i, (_, _, href))| (i.to_string(), (*href).to_string()))
            .collect(),
        ..Snapshot::default()
    }
}

fn desktop(label: &str, app: &str, in_content: bool) -> Candidate {
    Candidate {
        id: "1".into(),
        role: "button".into(),
        label: label.into(),
        frame: (0, 0, 40, 20),
        actions: vec!["AXPress".into()],
        enabled: true,
        app: app.into(),
        in_content,
        ..Candidate::default()
    }
}

const ACCOUNT_LABELS: &[&str] = &[
    "upvote",
    "downvote",
    "Like",
    "Unlike",
    "Follow",
    "Unfollow",
    "Star",
    "Unstar",
    "hide",
    "flag",
    "Report",
    "Save",
    "Subscribe",
    "▲ upvote",
    "Like 12",
    "Follow @jane",
    "Star 1.2k",
    "Save post",
    "flag comment",
    "  UPVOTE  ",
    "♥ Like",
];
const PLAIN_LABELS: &[&str] = &[
    "Saved articles",
    "Follow-up",
    "Follow-up questions",
    "Follow up",
    "Hide sidebar",
    "Hide toolbar",
    "Hide details",
    "Show more",
    "Stories you might like",
    "People to follow this week",
    "Starred repositories",
    "Liked videos",
    "Things I like",
    "Stargazers",
    "Followers",
    "Hidden",
    "Saved",
    "Flagged",
    "Report card 2025 results",
    "Home",
    "",
];

#[test]
fn test_short_account_change_labels_are_caught() {
    for label in ACCOUNT_LABELS {
        assert!(is_account_change(label, "", ""), "{label}");
    }
}

#[test]
fn test_navigation_headings_and_ui_toggles_are_not() {
    for label in PLAIN_LABELS {
        assert!(!is_account_change(label, "", ""), "{label}");
    }
}

#[test]
fn test_account_change_endpoints_are_caught_whatever_the_label() {
    for href in [
        "https://news.ycombinator.com/vote?id=4123&how=up&auth=abc",
        "https://news.ycombinator.com/hide?id=4123&goto=news",
        "https://news.ycombinator.com/flag?id=4123",
        "https://github.com/owner/repo/star",
        "https://example.com/api/upvote",
        "https://example.com/users/jane/follow",
        "https://example.com/post/9/like",
        "https://example.com/like.php?id=9",
        "https://example.com/item?id=9&action=vote",
        "/vote?id=1&how=up",
    ] {
        assert!(is_account_change("", href, ""), "{href}");
        assert!(is_account_change("story title that reads like anything", href, ""), "{href}");
    }
}

#[test]
fn test_reading_links_are_not() {
    for href in [
        "https://news.ycombinator.com/item?id=4123",
        "https://github.com/owner/repo/stargazers",
        "https://example.com/blog/follow-us",
        "https://en.wikipedia.org/wiki/Star",
        "https://example.com/search?q=like",
        "https://example.com/saved",
        "https://example.com/followers",
        "https://example.com/hidden-gems",
        "",
        "not a url",
        "https://example.com/votes/results",
    ] {
        assert!(!is_account_change("Read more", href, ""), "{href}");
    }
}

#[test]
fn test_non_strings_fail_closed() {
    // Python's None / 42 labels have no Rust spelling (labels are &str); a missing href is "".
    assert!(!is_account_change("Read more", "", ""));
    // a URL urlsplit rejects (ValueError) fails closed, as the Python's except does
    assert!(is_account_change("Read more", "https://[::1/path", ""));
    assert!(is_account_change("Read more", "https://[not-an-ip]/x", ""));
}

#[test]
fn test_browser_candidates_need_confirmation_by_label_and_by_href() {
    let s = page(&[
        ("link", "upvote", "https://news.ycombinator.com/vote?id=1&how=up"),
        ("link", "Show HN: a thing", "https://news.ycombinator.com/item?id=1"),
        ("link", "hide", "https://news.ycombinator.com/hide?id=1"),
        ("link", "", "https://news.ycombinator.com/vote?id=2&how=up"), // the arrow has no name
        ("link", "Saved articles", "https://news.ycombinator.com/favorites"),
        ("button", "Delete account", ""),
    ]);
    let hrefs: HashMap<&str, &str> = [
        ("0", "https://news.ycombinator.com/vote?id=1&how=up"),
        ("2", "https://news.ycombinator.com/hide?id=1"),
        ("3", "https://news.ycombinator.com/vote?id=2&how=up"),
        ("4", "https://news.ycombinator.com/favorites"),
    ]
    .into_iter()
    .collect();
    let asked: Vec<String> = s
        .elements
        .iter()
        .filter(|c| needs_confirmation(c, hrefs.get(c.id.as_str()).copied().unwrap_or("")))
        .map(|c| c.label.clone())
        .collect();
    assert_eq!(asked, ["upvote", "hide", "", "Delete account"]);
}

#[test]
fn test_desktop_save_and_hide_stay_unasked_but_sensitive_verbs_still_ask() {
    // confirm-only-dangerous: the Mac lane's "Save" / "Hide Toolbar" / Mail's "Flag" are not account changes.
    for label in ["Save", "Hide", "Flag", "Like", "Star"] {
        assert!(!needs_confirmation(&desktop(label, "TextEdit", false), ""), "{label}");
    }
    assert!(needs_confirmation(&desktop("Delete", "TextEdit", false), ""));
    assert!(needs_confirmation(&desktop("Send", "TextEdit", false), ""));
    // Web content under an AXWebArea in a desktop browser is a page: the same rule as eve's own browser.
    assert!(needs_confirmation(&desktop("upvote", "Safari", true), ""));
    assert!(needs_confirmation(&desktop("Like", "Safari", true), ""));
}

#[test]
fn test_rank_drops_account_changes_from_the_browser_menu_unless_allowed() {
    let s = page(&[
        ("link", "upvote", ""),
        ("link", "Show HN: a thing", ""),
        ("link", "Follow", ""),
        ("link", "Saved articles", ""),
        ("button", "Hide sidebar", ""),
    ]);
    let (kept, _) = rank(&s, "", 20);
    let mut got = labels(&kept);
    got.sort();
    assert_eq!(got, ["Hide sidebar", "Saved articles", "Show HN: a thing"]);
    let (kept, _) = rank_candidates(&s, "", 20, Kinds::Pressable, false, true);
    let mut got = labels(&kept);
    got.sort();
    assert_eq!(got, ["Follow", "Hide sidebar", "Saved articles", "Show HN: a thing", "upvote"]);
}

// ---- test_side_effect_precision.py: only verb-shaped controls ask ---------------------------------

fn pcand(label: &str, role: &str, app: &str, in_content: bool, cid: &str) -> Candidate {
    Candidate {
        id: cid.into(),
        role: role.into(),
        label: label.into(),
        frame: (10, 20 * cid.parse::<i64>().unwrap(), 60, 16),
        actions: vec!["AXPress".into()],
        enabled: true,
        app: app.into(),
        in_content,
        ..Candidate::default()
    }
}

fn link(label: &str) -> Candidate {
    pcand(label, "link", "browser", false, "1")
}

const PLAIN: &[&str] = &[
    "Star Wars",
    "Star Wars: Andor",
    "Like New",
    "Save $5",
    "Save 20%",
    "Save Your Tears",
    "Hide and Seek",
    "Flag of France",
    "Save changes",
    "Save for later",
    "Like a Rolling Stone",
    "Star Trek",
    "Follow the Money",
    "Save 20 percent",
    "Hide & Seek",
];
const REPORT_TITLES: &[&str] = &["Report a problem", "Report: inflation cools", "Report card 2025"];

#[test]
fn test_titles_and_shop_labels_are_not_account_changes() {
    for label in PLAIN {
        assert!(!is_account_change(label, "", ""), "{label}");
        assert!(!is_account_change(label, "", "button"), "{label}");
        assert!(!needs_confirmation(&link(label), ""), "{label}"); // a link in eve's browser
        assert!(!needs_confirmation(&pcand(label, "button", "browser", false, "1"), ""), "{label}");
        assert!(!needs_confirmation(&pcand(label, "link", "Safari", true, "1"), ""), "{label}");
    }
}

#[test]
fn test_report_titles_are_not_account_changes() {
    for label in REPORT_TITLES {
        assert!(!is_account_change(label, "", "") && !is_account_change(label, "", "button"), "{label}");
    }
}

const VERB_SHAPED: &[&str] = &[
    "upvote",
    "Like",
    "Follow",
    "Star",
    "Save",
    "hide",
    "flag",
    "Report",
    "Subscribe",
    "▲ upvote",
    "♥ Like",
    "Like 12",
    "Star 1.2k",
    "Follow 3,401",
    "Save post",
    "Save this",
    "flag comment",
    "Follow @jane",
    "Like this post",
    "Unfollow @jane_doe",
    "Report this",
    "Report post",
    "Upvote 12",
];

#[test]
fn test_verb_shaped_labels_still_ask() {
    for label in VERB_SHAPED {
        assert!(is_account_change(label, "", ""), "{label}");
        assert!(needs_confirmation(&pcand(label, "button", "browser", false, "1"), ""), "{label}");
        assert!(needs_confirmation(&pcand(label, "link", "Google Chrome", true, "1"), ""), "{label}");
    }
}

#[test]
fn test_follow_a_named_person_asks_on_a_button_only() {
    // "Follow Jane Doe" is the aria-label of a follow button; as a link it reads as a title ("Follow the Money").
    assert!(needs_confirmation(&pcand("Follow Jane Doe", "button", "browser", false, "1"), ""));
    assert!(!needs_confirmation(&pcand("Follow Jane Doe", "link", "browser", false, "1"), ""));
    assert!(!needs_confirmation(&pcand("Star Wars", "button", "browser", false, "1"), ""));
    // follow-type verbs only
}

#[test]
fn test_a_link_titled_after_its_own_page_is_a_title() {
    assert!(!is_account_change("Like", "https://en.wikipedia.org/wiki/Like", "link"));
    assert!(!is_account_change("Hide", "https://www.imdb.com/find/?q=hide", "link"));
    assert!(!is_account_change("Star", "https://en.wikipedia.org/wiki/Star", "link"));
    assert!(!is_account_change("Save Your Tears", "https://music.example.com/song/save-your-tears", "link"));
    // a verb link that goes somewhere else still asks, and an account endpoint always does
    assert!(is_account_change("upvote", "https://news.ycombinator.com/x", "link"));
    assert!(is_account_change("hide", "https://news.ycombinator.com/hide?id=4123&goto=news", "link"));
    assert!(is_account_change("Star", "https://github.com/owner/repo/star", "link"));
}

#[test]
fn test_electron_app_chrome_stays_unasked() {
    for (app, label) in [
        ("Slack", "Save for later"),
        ("Slack", "Save"),
        ("Spotify", "Follow"),
        ("Spotify", "Like"),
        ("Discord", "Star"),
        ("Notion", "Follow"),
    ] {
        assert!(!needs_confirmation(&pcand(label, "button", app, true, "1"), ""), "{app} {label}");
    }
}

#[test]
fn test_web_content_in_a_real_browser_still_asks() {
    for app in ["Safari", "Google Chrome", "Arc", "Firefox", "Microsoft Edge", "Brave Browser"] {
        assert!(needs_confirmation(&pcand("Follow", "button", app, true, "1"), ""), "{app}");
        assert!(needs_confirmation(&pcand("", "link", app, true, "1"), VOTE), "{app}");
    }
}

#[test]
fn test_sensitive_labels_ask_in_every_app() {
    assert!(needs_confirmation(&pcand("Delete message", "button", "Slack", true, "1"), ""));
    assert!(needs_confirmation(&pcand("Send", "button", "Spotify", true, "1"), ""));
}

const VOTE: &str = "https://news.ycombinator.com/vote?id=1&how=up";

fn hn_page(seq: i64) -> Snapshot {
    let rows = vec![
        link_id("Show HN: a thing", "0"),
        link_id("", "1"),
        link_id("vote arrow", "2"),
        link_id("12 comments", "3"),
    ];
    Snapshot {
        seq,
        app: "browser".into(),
        bundle: "eve.browser".into(),
        title: "Hacker News".into(),
        pid: 1,
        elements: rows,
        context_lines: vec!["title: Hacker News".into()],
        node_count: 4,
        hrefs: [
            ("0", "https://news.ycombinator.com/item?id=1".to_string()),
            ("1", VOTE.to_string()),
            ("2", VOTE.replace("1&", "2&")),
            ("3", "https://news.ycombinator.com/item?id=1".to_string()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect(),
        ..Snapshot::default()
    }
}

fn link_id(label: &str, cid: &str) -> Candidate {
    pcand(label, "link", "browser", false, cid)
}

#[test]
fn test_the_snapshot_carries_each_controls_href() {
    let s = hn_page(1);
    assert!(s.href("1") == VOTE && s.href("0").ends_with("item?id=1") && s.href("9").is_empty());
    // a plain snapshot (fixtures, the Mac walk) has none, and an observed link destination is read when present
    let bare = Snapshot {
        seq: 1,
        app: "browser".into(),
        bundle: "eve.browser".into(),
        pid: 1,
        elements: vec![link_id("★ 1.2k", "4")],
        context_lines: vec![r#"link destination: {"id": "4", "url": "https://github.com/owner/repo/star"}"#.into()],
        ..Snapshot::default()
    };
    assert!(bare.href("4") == "https://github.com/owner/repo/star" && bare.href("5").is_empty());
    assert!(needs_confirmation(&bare.elements[0], &bare.href("4")));
    // an id written as a number reads as its str(), and a bad line is skipped
    let numeric = Snapshot {
        context_lines: vec![
            "link destination: {not json".into(),
            r#"link destination: {"id": 4, "url": "https://example.com/a"}"#.into(),
        ],
        ..Snapshot::default()
    };
    assert_eq!(numeric.href("4"), "https://example.com/a");
}

#[test]
fn test_rank_withholds_an_href_only_vote_link() {
    let (kept, _) = rank(&hn_page(1), "", 20);
    let mut got = labels(&kept);
    got.sort();
    assert_eq!(got, ["12 comments", "Show HN: a thing"]);
}

// ---- urllib.parse parity -----------------------------------------------------------------------

#[test]
fn test_urlsplit_parse_qsl_unquote_match_python() {
    let p = urlsplit("HTTPS://news.ycombinator.com/vote?id=1&how=up#frag");
    assert_eq!(
        (p.scheme.as_str(), p.netloc.as_str(), p.path.as_str(), p.query.as_str(), p.fragment.as_str()),
        ("https", "news.ycombinator.com", "/vote", "id=1&how=up", "frag")
    );
    let p = urlsplit("/vote?id=1");
    assert!(p.scheme.is_empty() && p.netloc.is_empty() && p.path == "/vote" && p.query == "id=1");
    let p = urlsplit("not a url");
    assert_eq!(p.path, "not a url");
    let p = urlsplit("  ja\nva:script");
    assert!(p.scheme == "java" && p.path == "script");
    assert_eq!(parse_qsl("a=1&b=&&c=x+y%21", false), [("a".to_string(), "1".to_string()), ("c".into(), "x y!".into())]);
    assert_eq!(parse_qsl("a=1&b=&c", true).len(), 3);
    assert_eq!(unquote("save-your%20tears"), "save-your tears");
    assert_eq!(unquote("%%41%zz%e2%82%ac%C3"), "%A%zz€\u{fffd}");
    assert_eq!(unquote("caf%C3%A9 é"), "café é");
    assert_eq!(unquote("plain"), "plain");
}

#[test]
fn test_fixture_dict_round_trip_keeps_numbers() {
    // the module's own helpers read Python's int()/float() coercions
    let s =
        snapshot_from_dict(&json!({"seq": "3", "pid": 7.9, "secs": "0.25", "elements": [], "focused": {}})).unwrap();
    assert!(s.seq == 3 && s.pid == 7 && s.secs == 0.25 && s.focused.is_none());
    assert!(snapshot_from_dict(&json!({"seq": null})).is_err());
    let _ = ax::DESTINATION_PREFIX;
}

// ---- test_ax_press_fallback.py: the live element rides the snapshot -------------------------------

#[test]
fn test_refs_ride_the_snapshot_and_never_reach_its_dict() {
    // REF stands for the live AXUIElement
    struct Ref;
    let node = json!({"role": "AXButton", "title": "Week", "frame": [10, 10, 40, 20], "actions": ["AXPress"],
                      "enabled": true, "children": []});
    let root = json!({"role": "AXWindow", "title": "w", "frame": [0, 0, 500, 500], "children": [node]});
    let raw = json!({"app": "Calendar", "bundle": "b", "pid": 42, "root": root});
    let shared = AxRef::new(Ref);
    let refs: HashMap<NodePath, AxRef> = [(vec![0], shared.clone())].into_iter().collect();
    let snap = snapshot_from_raw(&raw, 1, None, Some(&refs));
    assert_eq!(snap.elements.len(), 1);
    let c = &snap.elements[0];
    let got = snap.element_ref(&c.id).expect("the button's live element");
    assert!(std::sync::Arc::ptr_eq(&got.0, &shared.0)); // `is REF`
    assert_eq!(snap, snapshot_from_raw(&raw, 1, None, None)); // refs never change equality
    let dumped = serde_json::to_string(&snap.to_dict()).unwrap(); // and never reach the serialised form
    assert!(!dumped.contains("ref"), "{dumped}");
    assert!(snapshot_from_raw(&raw, 1, None, None).element_ref(&c.id).is_none());
}

// ---- test_side_effect_precision.py: the lane's gate and run_delegate ask by href ------------------

struct HnSenses(Snapshot);

impl eve_desktop_lanes::fast_lane::Senses for HnSenses {
    fn snapshot(&self) -> anyhow::Result<Snapshot> {
        Ok(self.0.clone())
    }
    fn text_visible(&self, _s: &str) -> anyhow::Result<bool> {
        Ok(false)
    }
    fn focused(&self) -> Option<Candidate> {
        None
    }
    fn frontmost_pid(&self) -> anyhow::Result<i64> {
        Ok(1)
    }
    fn screen_changed(&self) -> anyhow::Result<Option<bool>> {
        Ok(Some(true))
    }
}

#[derive(Default)]
struct HnEffectors {
    clicks: std::cell::RefCell<Vec<String>>,
}

impl eve_desktop_lanes::fast_lane::Effectors for HnEffectors {
    fn click_candidate(&self, c: &Candidate) -> anyhow::Result<String> {
        self.clicks.borrow_mut().push(c.label.clone());
        Ok(format!("clicked {} '{}'", c.role, c.label))
    }
    fn focus_and_type(&self, _c: &Candidate, _text: &str) -> anyhow::Result<String> {
        anyhow::bail!("the precision test never types")
    }
    fn press(&self, _key: &str) -> anyhow::Result<String> {
        anyhow::bail!("the precision test never presses a key")
    }
    fn settle(&self, _secs: f64) -> anyhow::Result<f64> {
        Ok(0.0)
    }
}

#[test]
fn test_gate_asks_for_an_href_only_account_link() {
    use eve_desktop_lanes::fast_lane::{gate, GateState, MenuItem, Thresholds};

    let c = hn_page(1).get("2").cloned().expect("the vote arrow");
    let item = MenuItem {
        key: "1".into(),
        kind: "click".into(),
        text: "link: vote arrow".into(),
        candidate: Some(c.clone()),
        payload: String::new(),
        href: VOTE.into(),
    };
    let choice = eve_decider::Choice {
        id: "1".into(),
        p_top: 1.0,
        margin: 1.0,
        confidence: 1.0,
        probabilities: [("1".to_string(), 1.0)].into_iter().collect(),
        k: 1,
        ..eve_decider::Choice::default()
    };
    let th = Thresholds::default();
    let kw = GateState { approve: None, approve_used: false, search_typed: false, dialog_open: false, reobserve_count: 0 };
    assert_eq!(gate(&choice, Some(&item), &th, &kw), ("confirm", "sensitive"));
    let no_href = MenuItem { href: String::new(), ..item.clone() };
    assert_eq!(gate(&choice, Some(&no_href), &th, &kw), ("act", "")); // no href, no tell
    let yes = GateState { approve: Some("vote arrow"), ..kw };
    assert_eq!(gate(&choice, Some(&item), &th, &yes), ("act", "")); // the human's yes
}

#[test]
fn test_the_lane_confirms_an_href_only_vote_link_instead_of_clicking_it() {
    use eve_desktop_lanes::fast_lane::{run_delegate, DelegateOptions, LaneStatus};

    let opts = || DelegateOptions { decide: "keyword".into(), allow_page_links: true, ..DelegateOptions::default() };
    let eff = HnEffectors::default();
    let r = run_delegate("click the vote arrow", &HnSenses(hn_page(1)), &eff, None, opts());
    assert!(r.status == LaneStatus::Confirm && r.line.starts_with("confirm: \"vote arrow\""), "{}", r.line);
    assert!(eff.clicks.borrow().is_empty());
    // the same page, a reading link: clicked as before
    let eff = HnEffectors::default();
    let r = run_delegate("open the comments", &HnSenses(hn_page(1)), &eff, None, opts());
    assert_eq!(*eff.clicks.borrow(), vec!["12 comments".to_string()], "{}", r.line);
}
