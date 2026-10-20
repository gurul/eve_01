//! The fast lane's senses: the frontmost window's accessibility tree as a bounded, code-owned candidate list
//! (fast_lane ranks it, the decider picks from it). Port of `ax_candidates.py`; its module docstring is the
//! contract and is kept here in substance.
//!
//! Contract
//! --------
//! [`ax_snapshot`] walks the focused window of one app and returns a [`Snapshot`]: every labelled control as a
//! [`Candidate`] in reading order (y, then x), tagged with what the lane must never guess — enabled, inside a
//! sheet or dialog, inside page content, a secure field, an editable field. [`rank_candidates`] turns a snapshot
//! plus an objective into a short menu: it drops what the lane may not offer (disabled, unlabelled, dialog,
//! secure, page links, and every [`is_sensitive`] label unless the caller allows it), scores the rest by a role
//! prior plus objective-token overlap, and caps the list. No app-specific boosts anywhere: the menu is code, the
//! pick is the model's, the judgement is code again.
//!
//! [`SENSITIVE_LABEL`] is word-bounded and fail-closed: any label that carries a consequential verb ("Delete
//! Event", "Send", "Don't Save", "OK", "Remove filter", "Export as PDF") is sensitive wherever the word sits, and
//! the lane answers `confirm` for it so the planner can ask the human. "Postcode", "Composer", "Dispatcher",
//! "Downloads" are not: the verb must be a whole word.
//!
//! [`needs_confirmation`] is that rule plus one for page controls only (eve's own browser, or web content in a Mac
//! browser — not an Electron app's own chrome): a verb-shaped label (the bare account verb, the verb and a count,
//! what it acts on or a @handle: "upvote", "Like 12", "Save post", "Follow @jane") or a link whose endpoint is one
//! (/vote, /follow, /star, /like, /flag, /hide) changes the signed-in account, so it asks too. Titles and shop
//! labels ("Star Wars", "Like New", "Save 20%", "Save changes"), a link to a page named after its label
//! (/wiki/Like), "Saved articles", "Hide sidebar" and the Mac lane's own "Save" do not: only dangerous acts ask.
//! [`Snapshot::href`] carries each page control's href to it.
//!
//! Raw walk shape (the injectable backend, what fixtures store)
//! -------------------------------------------------------------
//! `walk(pid, max_nodes, budget_secs)` returns one JSON value:
//!
//! ```text
//! {"app": str, "bundle": str, "pid": int, "title": str, "fullscreen": bool,
//!  "node_count": int, "truncated": bool, "secs": float,
//!  "root": <node> | null}          # null when the app has no window
//!
//! <node> = {"role": "AXButton", "subrole": "", "title": "Today", "description": "",
//!           "value": str | int | float | bool | null, "frame": [x, y, w, h] | null,
//!           "actions": ["AXPress"], "enabled": bool, "focused": bool,
//!           "children": [<node>, ...], "ancestor_roles": ["AXWindow", "AXGroup"]}
//! ```
//!
//! Frames are points with a top-left origin (click coordinates). [`snapshot_from_raw`] derives the same Snapshot
//! from the same dump every time. A live element of the dump is keyed by its [`NodePath`] (child indices from the
//! root), Python's `id(node)`.
//!
//! The real walker ([`ax_walk`]) uses AXUIElementCopyMultipleAttributeValues — one IPC per node for role, subrole,
//! title, description, value, position, size, enabled, focused and children — plus one AXUIElementCopyActionNames
//! only where an action can change the answer (a node whose role is not already pressable, outside page content,
//! not a container). It visits content subtrees (AXWebArea, AXScrollArea, AXContentList) before chrome
//! (`content_first=true`, the plan's default) so a truncated walk of a web page still carries the page, and stops
//! at `max_nodes` or `budget_secs` with `truncated=true`.
//!
//! Measured on this Mac (2026-09-21, five walks each): Calendar month view 124 nodes, 0.10-0.20 s, 59 labelled
//! pressables (radio buttons carry AXValue 1 when selected; events are AXStaticText with AXPress; the three
//! untitled 16x16 buttons are the traffic lights). System Settings 167 nodes, 0.04-0.11 s, 38 pressables after
//! the row/cell de-duplication. Safari on a Wikipedia article 3632 nodes, 0.36-0.43 s, 79 pressables of which 9
//! are chrome. The 0.5 s default budget covers all three; a walk capped at 800 nodes on that page reports
//! truncated=true and, with content first, no chrome. An app with no window reports 0 nodes.

use std::any::Any;
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, LazyLock};

use serde_json::Value;

pub const DEFAULT_MAX_NODES: usize = 4000;
pub const DEFAULT_BUDGET_SECS: f64 = 0.5;
pub const ATTRIBUTES: [&str; 10] = [
    "AXRole",
    "AXSubrole",
    "AXTitle",
    "AXDescription",
    "AXValue",
    "AXPosition",
    "AXSize",
    "AXEnabled",
    "AXFocused",
    "AXChildren",
];
/// AX role → plain words, in Python's dict order.
pub const ROLE_NAMES: &[(&str, &str)] = &[
    ("AXButton", "button"),
    ("AXLink", "link"),
    ("AXCheckBox", "checkbox"),
    ("AXRadioButton", "radio button"),
    ("AXMenuItem", "menu item"),
    ("AXMenuButton", "menu button"),
    ("AXPopUpButton", "pop up button"),
    ("AXTab", "tab"),
    ("AXTextField", "text field"),
    ("AXSecureTextField", "text field"),
    ("AXSearchField", "search field"),
    ("AXTextArea", "text area"),
    ("AXComboBox", "combo box"),
    ("AXRow", "row"),
    ("AXCell", "cell"),
    ("AXDisclosureTriangle", "disclosure triangle"),
    ("AXSlider", "slider"),
    ("AXImage", "image"),
    ("AXStaticText", "static text"),
    ("AXSwitch", "toggle"),
    ("AXToggle", "toggle"),
];
/// Roles the lane may click by role alone (an AXPress action makes any node pressable).
pub const PRESSABLE_ROLES: &[&str] = &[
    "button",
    "link",
    "checkbox",
    "radio button",
    "menu item",
    "menu button",
    "pop up button",
    "tab",
    "toggle",
    "disclosure triangle",
    "row",
    "cell",
];
pub const EDITABLE_ROLES: &[&str] = &["text field", "search field", "text area", "combo box"];
pub const ROLE_PRIOR: &[(&str, i64)] = &[
    ("button", 3),
    ("tab", 3),
    ("radio button", 3),
    ("checkbox", 3),
    ("menu item", 3),
    ("menu button", 3),
    ("pop up button", 3),
    ("row", 2),
    ("cell", 2),
    ("toggle", 2),
    ("disclosure triangle", 2),
    ("link", 1),
    ("image", 1),
    ("static text", 1),
];
pub const CONTENT_ROLES: &[&str] = &["AXWebArea", "AXContentList"];
pub const CONTENT_FIRST_ROLES: &[&str] = &["AXWebArea", "AXScrollArea", "AXContentList"];
/// PLAN §1a: content subtree before chrome.
pub const CONTENT_FIRST: bool = true;
pub const DIALOG_SUBROLES: &[&str] = &["AXDialog", "AXSystemDialog", "AXFloatingWindow"];
/// Containers never carry an action worth an IPC; their children do.
pub const CONTAINER_ROLES: &[&str] = &[
    "AXWindow",
    "AXGroup",
    "AXScrollArea",
    "AXSplitGroup",
    "AXToolbar",
    "AXList",
    "AXOutline",
    "AXTable",
    "AXWebArea",
    "AXLayoutArea",
    "AXLayoutItem",
    "AXTabGroup",
    "AXSplitter",
    "AXScrollBar",
    "AXBrowser",
    "AXDrawer",
    "AXSheet",
    "AXUnknown",
    "AXContentList",
    "AXGrowArea",
    "AXRulerMarker",
    "AXRuler",
    "AXMenuBar",
];
pub const SKIP_DESCEND_ROLES: &[&str] = &["AXScrollBar", "AXMenuBar", "AXRuler"];
pub const STOP_WORDS: &[&str] = &[
    "the", "and", "for", "with", "into", "from", "this", "that", "then", "now", "please", "open", "show", "switch",
    "view", "see", "check", "find", "click", "tap", "press", "select", "choose", "make", "turn", "set", "get", "put",
    "want", "need", "use", "let", "can", "you", "your", "its", "one", "all", "some", "any",
];
pub const MAX_VALUE_CHARS: usize = 40;
pub const MAX_CONTEXT_LINES: usize = 8;
pub const MAX_CONTEXT_TEXTS: usize = 6;
pub const MAX_DIALOG_CHARS: usize = 80;

/// Python `_URL`: `(?:\b[a-z][a-z0-9+.\-]*://\S+|\bwww\.\S+)`, IGNORECASE.
pub static URL_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)(?:\b[a-z][a-z0-9+.\-]*://[^\s\x1c-\x1f]+|\bwww\.[^\s\x1c-\x1f]+)").expect("_URL")
});
/// Python `_SPACES`: `\s+`.
/// Python's `\s` also matches U+001C..U+001F (`str.isspace()`), which Rust's `\s` does not: the class adds them.
pub static SPACES_RE: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"[\s\x1c-\x1f]+").expect("_SPACES"));

/// The consequential verbs and phrases, as Python regex fragments, verbatim (`_SENSITIVE_PHRASES`).
pub const SENSITIVE_PHRASES: &[&str] = &[
    "delete",
    "remove",
    "trash",
    "erase",
    "empty",
    "discard",
    "don[’'`]?t\\s+save",
    "replace",
    "overwrite",
    "send",
    "submit",
    "post",
    "reply",
    "pay",
    "buy",
    "purchase",
    "subscribe",
    "checkout",
    "check\\s+out",
    "sign\\s+in",
    "signin",
    "log\\s+in",
    "login",
    "log\\s+out",
    "logout",
    "sign\\s+out",
    "signout",
    "password",
    "authori[sz]e",
    "permissions?",
    "allow",
    "captcha",
    "upload",
    "share",
    "export",
    "install",
    "uninstall",
    "quit",
    "close\\s+(?:window|tab|all)",
    "shut\\s+down",
    "shutdown",
    "restart",
    "reset",
    "forget",
    "disconnect",
    "revoke",
    "eject",
    "call",
    "facetime",
    "decline",
    "accept",
    "agree",
    "continue",
    "ok",
    "okay",
    "yes",
    "apply",
    "archive",
    "unsubscribe",
    "block",
    "report",
    "move\\s+to\\s+trash",
    "system\\s+settings",
    "security",
    // 2026-09-21: the verbs the request-level gates already knew (task_router.CONSEQUENTIAL, the risky_action
    // noul) and this table did not. They were masked while the lane only pressed a control the human had named
    // word for word; a plan executor presses controls nobody named. "Forward" (the browser's and Finder's
    // navigation button) and "Format" (Notes' toolbar) are left out on purpose: whole phrases cover the risky use.
    "place\\s+(?:an?\\s+|your\\s+|my\\s+)?order",
    "order\\s+now",
    "complete\\s+(?:order|purchase)",
    "confirm",
    "book",
    "publish",
    "transfer",
    "clear\\s+(?:history|all|data|cache|browsing\\s+data)",
    "cancel\\s+(?:subscription|membership|order|plan)",
    "merge\\s+pull\\s+request",
    "squash\\s+and\\s+merge",
    "rebase\\s+and\\s+merge",
    "format\\s+(?:disk|drive|volume)",
    "turn\\s+o(?:n|ff)",
    "approve",
    "kill",
    "add\\s+to\\s+(?:cart|bag|basket)",
    "mark\\s+as\\s+(?:spam|junk)",
    "spam",
    "junk",
    "revert",
    "sign\\s+up",
    "signup",
    "create\\s+(?:an?\\s+)?account",
    "donate",
    "withdraw",
    "deposit",
    "bid",
];
/// Word-bounded on both sides so "Postcode", "Composer", "Dispatcher", "Okay dokey"-style prefixes never fire on a
/// fragment, while "Delete Event", "Share…", "OK" and "Don't Save" always do. Python:
/// `(?<![a-z0-9])(?:<phrases>)(?![a-z0-9])`, IGNORECASE.
pub static SENSITIVE_LABEL: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    let pattern = format!("(?i)(?<![a-z0-9])(?:{})(?![a-z0-9])", sensitive_alternation());
    fancy_regex::Regex::new(&pattern).expect("SENSITIVE_LABEL")
});

/// The phrases as one alternation, with Python's `\s` (which also matches U+001C..U+001F) spelled out.
fn sensitive_alternation() -> String {
    SENSITIVE_PHRASES.join("|").replace("\\s", "[\\s\\x1c-\\x1f]")
}

/// `SENSITIVE_LABEL.search(text) is not None` without a backtracking engine: the two lookarounds become a consumed
/// boundary character or a text edge, which decides the same existence question. fancy-regex's VM gives up past
/// its backtrack limit on a long label, where Python's re never does.
static SENSITIVE_PRESENT: LazyLock<regex::Regex> = LazyLock::new(|| {
    let pattern = format!("(?i)(?:^|[^a-z0-9])(?:{})(?:[^a-z0-9]|$)", sensitive_alternation());
    regex::Regex::new(&pattern).expect("SENSITIVE_PRESENT")
});

// Account-changing page controls: plain-looking links that vote, follow, star, hide or save as the signed-in
// user. The verb set and the short-label / endpoint split are adapted from LayaBrowse (src/laya_voice_browser/
// safety.py, MIT, Copyright (c) 2026 Aryan Bhujade). The first-word anchor, the chrome nouns and the last-segment
// endpoint match are eve's, so "Follow-up", "Hide sidebar", "Stories you might like" and /wiki/Star stay plain.
pub const ACCOUNT_VERBS: &[&str] = &[
    "upvote",
    "downvote",
    "vote",
    "unvote",
    "like",
    "unlike",
    "follow",
    "unfollow",
    "star",
    "unstar",
    "hide",
    "unhide",
    "flag",
    "unflag",
    "report",
    "save",
    "unsave",
    "subscribe",
    "unsubscribe",
    "bookmark",
];
/// After the verb, a word that names a piece of the page itself makes it a view toggle, not an account change:
/// "Hide sidebar", "Hide 12 replies", "Save as PDF", "Follow up".
pub const CHROME_NOUNS: &[&str] = &[
    "sidebar",
    "toolbar",
    "panel",
    "pane",
    "menu",
    "details",
    "preview",
    "navigation",
    "nav",
    "controls",
    "options",
    "filters",
    "keyboard",
    "banner",
    "header",
    "footer",
    "password",
    "replies",
    "comments",
    "thread",
    "images",
    "image",
    "map",
    "as",
    "up",
    "all",
    "more",
    "less",
];
/// Endpoints match the LAST path segment, lowercase as the action URLs are ("/vote", "/owner/repo/star"); a page
/// title like /wiki/Star is capitalised and /stargazers, /followers, /follow-us are other words. "report" is left
/// out: /report is as often a dashboard. The label rule still catches a "Report" link. (`_ACCOUNT_ENDPOINTS`:
/// ACCOUNT_VERBS minus "report".)
pub const ACCOUNT_ENDPOINTS: &[&str] = &[
    "upvote",
    "downvote",
    "vote",
    "unvote",
    "like",
    "unlike",
    "follow",
    "unfollow",
    "star",
    "unstar",
    "hide",
    "unhide",
    "flag",
    "unflag",
    "save",
    "unsave",
    "subscribe",
    "unsubscribe",
    "bookmark",
];
pub const ACTION_KEYS: &[&str] = &["action", "do", "op", "act", "cmd", "method"];
/// Python `_ENDPOINT_EXT`: `\.(?:php|json|html?|aspx?|cgi|jsp)$`.
pub static ENDPOINT_EXT: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\.(?:php|json|html?|aspx?|cgi|jsp)$").expect("_ENDPOINT_EXT"));
/// Python `_LEAD_SYMBOLS`: `^[^\w@#]+`.
pub static LEAD_SYMBOLS: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^[^\w@#]+").expect("_LEAD_SYMBOLS"));
pub const TRAIL_PUNCT: &str = ".,:;!?…";
/// The shapes an account control's label takes after its verb. A count is digits with an optional k/m ("12",
/// "1.2k", "3,401"), never a price or a percentage. Python `_COUNT`: `^\d[\d,.]*[km]?$`, IGNORECASE.
pub static COUNT_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(?i)^\d[\d,.]*[km]?$").expect("_COUNT"));
/// Python `_HANDLE`: `^@\w[\w.\-]*$`.
pub static HANDLE_RE: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"^@\w[\w.\-]*$").expect("_HANDLE"));
pub const ACTED_ON: &[&str] = &[
    "this",
    "post",
    "comment",
    "story",
    "reply",
    "answer",
    "question",
    "video",
    "tweet",
    "repo",
    "repository",
    "photo",
    "track",
    "song",
    "playlist",
    "channel",
    "page",
    "link",
    "item",
];
/// Verbs whose object is a person or channel: "Follow Jane Doe" is a follow button's aria-label. As a link the
/// same shape is a title ("Follow the Money"), so this form counts on a button only.
pub const PERSON_VERBS: &[&str] = &["follow", "unfollow", "subscribe", "unsubscribe"];
/// Python `_SLUG_WORDS`: `[a-z0-9]+`.
pub static SLUG_WORDS: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"[a-z0-9]+").expect("_SLUG_WORDS"));
/// Mac apps whose web content (an AXWebArea) is a web page. Electron apps (Slack, Spotify, Discord, Notion) draw
/// their own chrome in an AXWebArea too; that is the app, so their "Follow" and "Save for later" stay unasked.
pub const BROWSER_APPS: &[&str] = &[
    "Safari",
    "Safari Technology Preview",
    "Google Chrome",
    "Google Chrome Canary",
    "Google Chrome Beta",
    "Chromium",
    "Arc",
    "Firefox",
    "Firefox Developer Edition",
    "Firefox Nightly",
    "Microsoft Edge",
    "Brave Browser",
    "Opera",
    "Vivaldi",
    "Orion",
    "DuckDuckGo",
    "Zen",
    "Zen Browser",
    "Dia",
    "Comet",
    "SigmaOS",
    "Waterfox",
    "LibreWolf",
];
/// browser_lane.snapshot_from_page's context line.
pub const DESTINATION_PREFIX: &str = "link destination: ";

/// Python `_SEQ`: the last snapshot sequence number handed out.
static SEQ: AtomicI64 = AtomicI64::new(0);

/// The next `seq` for a snapshot (Python's `global _SEQ; _SEQ += 1`).
pub fn next_seq() -> i64 {
    SEQ.fetch_add(1, Ordering::SeqCst) + 1
}

/// `(x, y, w, h)` in points, top-left origin.
pub type Rect = (i64, i64, i64, i64);

/// Child indices from a raw dump's root to one node: what keys a live element to its node (Python's `id(node)`).
pub type NodePath = Vec<usize>;

// ---- Python value semantics the port leans on ------------------------------------------------------

use serde_json::Map;

use crate::{casefold, py_is_space, py_len, py_split, py_strip, py_take};

/// Python's `str(v)` of the object a JSON value decodes to.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => eve_decider::py_repr(other),
    }
}

/// Python's `bool(v)`.
fn py_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => match (n.as_i64(), n.as_u64()) {
            (Some(i), _) => i != 0,
            (None, Some(u)) => u != 0,
            _ => n.as_f64().is_some_and(|f| f != 0.0),
        },
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(m) => !m.is_empty(),
    }
}

/// Python's `int(v)`: None where Python raises.
fn py_int(v: &Value) -> Option<i64> {
    match v {
        Value::Bool(b) => Some(i64::from(*b)),
        Value::Number(n) => n.as_i64().or_else(|| {
            let f = n.as_f64()?;
            let t = f.trunc();
            (f.is_finite() && t >= i64::MIN as f64 && t <= i64::MAX as f64).then_some(t as i64)
        }),
        Value::String(s) => py_strip(s).replace('_', "").parse::<i64>().ok(),
        _ => None,
    }
}

/// Python's `float(v)`: None where Python raises.
fn py_float(v: &Value) -> Option<f64> {
    match v {
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => n.as_f64(),
        Value::String(s) => py_strip(s).parse::<f64>().ok(),
        _ => None,
    }
}

/// Python's `str.rstrip()` with no argument.
fn py_rstrip(text: &str) -> &str {
    text.trim_end_matches(py_is_space)
}

/// Python's `repr(bool)`.
fn py_bool(b: bool) -> &'static str {
    if b {
        "True"
    } else {
        "False"
    }
}

/// Python's `round(x, 4)` (correctly rounded on the exact binary value, as Python's is).
fn round4(x: f64) -> f64 {
    crate::py_round_n(x, 4)
}

fn f64_value(f: f64) -> Value {
    serde_json::Number::from_f64(f).map(Value::Number).unwrap_or(Value::Null)
}

/// `node.get(key) or []` for a children list: anything that is not a list has none.
fn children_of(node: &Value) -> &[Value] {
    match node.get("children") {
        Some(Value::Array(items)) => items,
        _ => &[],
    }
}

// ---- urllib.parse, as far as the account rule needs it ------------------------------------------

/// Python's `urllib.parse.urlsplit` result.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UrlParts {
    pub scheme: String,
    pub netloc: String,
    pub path: String,
    pub query: String,
    pub fragment: String,
}

/// `urlsplit`'s ValueError, carrying Python's message ("Invalid IPv6 URL", or the bracketed host's own error).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UrlSplitError(pub(crate) String);

fn invalid_ipv6() -> UrlSplitError {
    UrlSplitError("Invalid IPv6 URL".to_string())
}

fn is_scheme_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')
}

/// Python 3.13 `_check_bracketed_host`.
fn check_bracketed_host(hostname: &str) -> Result<(), UrlSplitError> {
    if let Some(rest) = hostname.strip_prefix('v') {
        // \Av[a-fA-F0-9]+\..+\z
        let hex_len = rest.chars().take_while(|c| c.is_ascii_hexdigit()).count();
        let after = &rest[hex_len..];
        let ok = hex_len > 0 && after.starts_with('.') && after.len() > 1 && !after[1..].contains('\n');
        return if ok { Ok(()) } else { Err(UrlSplitError("IPvFuture address is invalid".to_string())) };
    }
    // ipaddress.ip_address: IPv6 only (an IPv4 address cannot be in brackets); a zone id is allowed.
    if hostname.parse::<std::net::Ipv4Addr>().is_ok() {
        return Err(UrlSplitError("An IPv4 address cannot be in brackets".to_string()));
    }
    let not_an_address =
        || UrlSplitError(format!("{} does not appear to be an IPv4 or IPv6 address", crate::desktop_helpers::py_repr(hostname)));
    let addr = hostname.split_once('%').map_or(hostname, |(a, _)| a);
    if hostname.split_once('%').is_some_and(|(_, zone)| zone.is_empty()) {
        return Err(not_an_address());
    }
    addr.parse::<std::net::Ipv6Addr>().map(|_| ()).map_err(|_| not_an_address())
}

/// Python 3.13 `_check_bracketed_netloc`.
fn check_bracketed_netloc(netloc: &str) -> Result<(), UrlSplitError> {
    let hostname_and_port = netloc.rsplit_once('@').map_or(netloc, |(_, h)| h);
    let hostname = match hostname_and_port.split_once('[') {
        Some((before, bracketed)) => {
            if !before.is_empty() {
                return Err(invalid_ipv6());
            }
            let (hostname, port) = bracketed.split_once(']').unwrap_or((bracketed, ""));
            if !port.is_empty() && !port.starts_with(':') {
                return Err(invalid_ipv6());
            }
            hostname
        }
        None => hostname_and_port.split_once(':').map_or(hostname_and_port, |(h, _)| h),
    };
    check_bracketed_host(hostname)
}

/// Python 3.13 `urlsplit(url)` with its ValueError.
pub(crate) fn try_urlsplit(url: &str) -> Result<UrlParts, UrlSplitError> {
    // lstrip(_WHATWG_C0_CONTROL_OR_SPACE), then drop every tab, CR and LF.
    let url: String =
        url.trim_start_matches(|c: char| c <= ' ').chars().filter(|c| !matches!(c, '\t' | '\r' | '\n')).collect();
    let mut scheme = String::new();
    let mut rest: &str = &url;
    if let Some(i) = url.find(':') {
        let first = url.chars().next();
        if i > 0 && first.is_some_and(|c| c.is_ascii_alphabetic()) && url[..i].chars().all(is_scheme_char) {
            scheme = url[..i].to_ascii_lowercase();
            rest = &url[i + 1..];
        }
    }
    let mut netloc = String::new();
    if let Some(after) = rest.strip_prefix("//") {
        let delim = after.find(['/', '?', '#']).unwrap_or(after.len());
        netloc = after[..delim].to_string();
        rest = &after[delim..];
        let (open, close) = (netloc.contains('['), netloc.contains(']'));
        if open != close {
            return Err(invalid_ipv6());
        }
        if open && close {
            check_bracketed_netloc(&netloc)?;
        }
    }
    let (rest, fragment) = rest.split_once('#').unwrap_or((rest, ""));
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    Ok(UrlParts { scheme, netloc, path: path.to_string(), query: query.to_string(), fragment: fragment.to_string() })
}

/// Python's `urllib.parse.urlsplit(url)` (scheme lower-cased, no validation, never fails).
pub fn urlsplit(url: &str) -> UrlParts {
    try_urlsplit(url).unwrap_or_default()
}

/// Python's `urllib.parse.parse_qsl(query, keep_blank_values=keep_blank)`.
pub fn parse_qsl(query: &str, keep_blank: bool) -> Vec<(String, String)> {
    let unquote_plus = |s: &str| unquote(&s.replace('+', " "));
    let mut out = Vec::new();
    if query.is_empty() {
        return out;
    }
    for name_value in query.split('&') {
        if name_value.is_empty() {
            continue;
        }
        let (name, value) = name_value.split_once('=').unwrap_or((name_value, ""));
        if !value.is_empty() || keep_blank {
            out.push((unquote_plus(name), unquote_plus(value)));
        }
    }
    out
}

/// Python's `unquote_to_bytes` on an ASCII run: `%` and two hex digits is a byte, anything else is literal.
fn unquote_ascii(run: &str) -> String {
    let bytes = run.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 3 <= bytes.len() && bytes[i + 1..i + 3].iter().all(u8::is_ascii_hexdigit) {
            if let Ok(b) = u8::from_str_radix(&run[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Python's `urllib.parse.unquote(text)`: percent escapes decoded as UTF-8 with replacement, one ASCII run at a
/// time (non-ASCII text passes through untouched, as Python's `_generate_unquoted_parts` does).
pub fn unquote(text: &str) -> String {
    if !text.contains('%') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut start = 0;
    let mut in_ascii = None::<bool>;
    for (i, c) in text.char_indices() {
        let ascii = c.is_ascii();
        match in_ascii {
            Some(prev) if prev != ascii => {
                let run = &text[start..i];
                if prev {
                    out.push_str(&unquote_ascii(run));
                } else {
                    out.push_str(run);
                }
                start = i;
            }
            _ => {}
        }
        in_ascii = Some(ascii);
    }
    let run = &text[start..];
    if in_ascii == Some(true) {
        out.push_str(&unquote_ascii(run));
    } else {
        out.push_str(run);
    }
    out
}

// ---- the sensitive and account rules -------------------------------------------------------------

/// True when a consequential verb is a whole word anywhere in the label; only an empty (or all-space) label is not.
/// (Python's fail-closed branch for a regex failure has no counterpart: the automaton cannot fail at match time.)
pub fn is_sensitive(label: &str) -> bool {
    let text = py_strip(label);
    if text.is_empty() {
        return false;
    }
    SENSITIVE_PRESENT.is_match(text)
}

/// Python `_verb_shaped`: `words` (lead symbols gone) is an account verb and nothing, a count, what it acts on, or
/// a @handle; or, on a button, a follow-type verb and a capitalised name of at most two words.
pub fn verb_shaped(words: &[String], role: &str) -> bool {
    if words.is_empty() || words.len() > 3 {
        return false;
    }
    let trail = |c: char| TRAIL_PUNCT.contains(c);
    let norm: Vec<String> = words.iter().map(|w| casefold(w).trim_end_matches(trail).to_string()).collect();
    let (verb, rest) = (norm[0].as_str(), &norm[1..]);
    if !ACCOUNT_VERBS.contains(&verb) || rest.iter().any(|w| CHROME_NOUNS.contains(&w.as_str())) {
        return false;
    }
    if rest.is_empty() {
        return true;
    }
    if rest.len() == 1 && COUNT_RE.is_match(&rest[0]) {
        return true;
    }
    if rest.iter().all(|w| ACTED_ON.contains(&w.as_str()) || HANDLE_RE.is_match(w)) && rest.len() <= 2 {
        return true;
    }
    role == "button"
        && PERSON_VERBS.contains(&verb)
        && words[1..].iter().all(|w| {
            let first_upper = w.chars().next().is_some_and(char::is_uppercase);
            let bare = w.trim_end_matches(trail);
            first_upper && !bare.is_empty() && bare.chars().all(char::is_alphabetic)
        })
}

/// The last path segment: `path.rstrip("/").rsplit("/", 1)[-1]`.
fn last_segment(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    trimmed.rsplit_once('/').map_or(trimmed, |(_, last)| last)
}

/// Python `_titles_its_destination`: the link goes to a page named after its own label (/wiki/Like for "Like",
/// ?q=hide for "Hide"). That is a title in a result list, not an action, however verb-like the label.
pub fn titles_its_destination(words: &[String], parts: &UrlParts) -> bool {
    let joined = casefold(&words.join(" "));
    let label: Vec<&str> = SLUG_WORDS.find_iter(&joined).map(|m| m.as_str()).collect();
    if label.is_empty() {
        return false;
    }
    let mut names = vec![unquote(last_segment(&parts.path))];
    names.extend(parse_qsl(&parts.query, false).into_iter().map(|(_, v)| v));
    names.iter().any(|name| {
        let folded = casefold(name);
        let slug: Vec<&str> = SLUG_WORDS.find_iter(&folded).map(|m| m.as_str()).collect();
        slug.len() >= label.len() && slug[..label.len()] == label[..]
    })
}

/// True when a page control changes the signed-in account: a link whose endpoint is an account verb, or a
/// verb-shaped label (leading ▲ ♥ ☆ ignored) that is not a link to a page named after it. A missing href is "".
pub fn is_account_change(label: &str, href: &str, role: &str) -> bool {
    let stripped = LEAD_SYMBOLS.replace(py_strip(label), "");
    let words: Vec<String> = py_split(&stripped).map(str::to_string).collect();
    let href = py_strip(href);
    if !href.is_empty() {
        // same as is_sensitive: a parse failure never lets a click through
        let Ok(parts) = try_urlsplit(href) else {
            return true;
        };
        let last = ENDPOINT_EXT.replace(last_segment(&parts.path), "");
        if ACCOUNT_ENDPOINTS.contains(&last.as_ref()) {
            return true;
        }
        if parse_qsl(&parts.query, true).iter().any(|(k, v)| {
            ACTION_KEYS.contains(&casefold(k).as_str()) && ACCOUNT_ENDPOINTS.contains(&casefold(v).as_str())
        }) {
            return true;
        }
        if titles_its_destination(&words, &parts) {
            return false;
        }
    }
    verb_shaped(&words, role)
}

/// `c` is on a web page: eve's own browser (app "browser"), or web content in a Mac browser (BROWSER_APPS).
pub fn is_web_page(c: &Candidate) -> bool {
    c.app == "browser" || (c.in_content && BROWSER_APPS.contains(&c.app.as_str()))
}

/// The lane asks the human before pressing `c`: a sensitive label anywhere, and on a web page (is_web_page) also
/// an account change by label or by `href`. The Mac lane's own "Save", "Hide" and "Flag", and an Electron app's
/// own chrome, stay unasked.
pub fn needs_confirmation(c: &Candidate, href: &str) -> bool {
    if is_sensitive(&c.label) {
        return true;
    }
    if !is_web_page(c) {
        return false;
    }
    is_account_change(&c.label, href, &c.role)
}

// ---- data ------------------------------------------------------------------------------------------

/// One control of a snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Candidate {
    /// reading-order index within THIS snapshot; never reused
    pub id: String,
    /// plain words: "button", "radio button", "search field", ...
    pub role: String,
    /// AXTitle / AXDescription / descendant static text — full
    pub label: String,
    /// "selected" | "checked" | "unchecked" | "expanded" | "collapsed" | text ≤ 40 | ""
    pub value: String,
    /// points, top-left origin (click coordinates)
    pub frame: Rect,
    pub actions: Vec<String>,
    pub enabled: bool,
    pub app: String,
    /// under an AXWebArea / AXContentList — page content, not chrome
    pub in_content: bool,
    /// under an AXSheet, or the window is a dialog
    pub in_dialog: bool,
    /// AXSecureTextField
    pub secure: bool,
    /// text field / search field / text area / combo box
    pub editable: bool,
}

/// `tuple(str(a) for a in v)`: a list's items, or a string's characters.
fn str_items(v: &Value) -> anyhow::Result<Vec<String>> {
    match v {
        Value::Array(items) => Ok(items.iter().map(py_str).collect()),
        Value::String(s) => Ok(s.chars().map(String::from).collect()),
        Value::Object(m) => Ok(m.keys().cloned().collect()),
        other => anyhow::bail!("TypeError: '{}' object is not iterable", py_type_name(other)),
    }
}

fn py_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

impl Candidate {
    /// `{"id", "role", "label", "value", "frame", "actions", "enabled", "app", "in_content", "in_dialog",
    /// "secure", "editable"}` in that order.
    pub fn to_dict(&self) -> Value {
        let (x, y, w, h) = self.frame;
        let mut m = Map::new();
        m.insert("id".into(), Value::from(self.id.clone()));
        m.insert("role".into(), Value::from(self.role.clone()));
        m.insert("label".into(), Value::from(self.label.clone()));
        m.insert("value".into(), Value::from(self.value.clone()));
        m.insert("frame".into(), Value::from(vec![x, y, w, h]));
        m.insert("actions".into(), Value::from(self.actions.clone()));
        m.insert("enabled".into(), Value::Bool(self.enabled));
        m.insert("app".into(), Value::from(self.app.clone()));
        m.insert("in_content".into(), Value::Bool(self.in_content));
        m.insert("in_dialog".into(), Value::Bool(self.in_dialog));
        m.insert("secure".into(), Value::Bool(self.secure));
        m.insert("editable".into(), Value::Bool(self.editable));
        Value::Object(m)
    }

    /// Python `Candidate.from_dict`: "id", "role" and "frame" are required (KeyError otherwise); the rest default.
    pub fn from_dict(d: &Value) -> anyhow::Result<Candidate> {
        let req = |key: &str| d.get(key).ok_or_else(|| anyhow::anyhow!("KeyError: '{key}'"));
        let frame_items = req("frame")?;
        let frame: Vec<i64> = match frame_items {
            Value::Array(items) => items
                .iter()
                .map(|v| py_int(v).ok_or_else(|| anyhow::anyhow!("ValueError: frame value {}", py_str(v))))
                .collect::<anyhow::Result<_>>()?,
            other => anyhow::bail!("TypeError: frame is a {}", py_type_name(other)),
        };
        let [x, y, w, h] = frame[..] else {
            anyhow::bail!("ValueError: frame has {} values, not 4", frame.len());
        };
        let opt_str = |key: &str| d.get(key).map(py_str).unwrap_or_default();
        let flag = |key: &str| d.get(key).is_some_and(py_truthy);
        Ok(Candidate {
            id: py_str(req("id")?),
            role: py_str(req("role")?),
            label: opt_str("label"),
            value: opt_str("value"),
            frame: (x, y, w, h),
            actions: match d.get("actions") {
                Some(v) => str_items(v)?,
                None => Vec::new(),
            },
            enabled: d.get("enabled").is_none_or(py_truthy),
            app: opt_str("app"),
            in_content: flag("in_content"),
            in_dialog: flag("in_dialog"),
            secure: flag("secure"),
            editable: flag("editable"),
        })
    }
}

impl eve_decider::OptionFields for Candidate {
    fn label(&self) -> Option<&str> {
        Some(&self.label)
    }
    fn role(&self) -> Option<&str> {
        Some(&self.role)
    }
    fn value(&self) -> Option<&str> {
        Some(&self.value)
    }
}

/// The live AXUIElement behind a candidate id (Python's `Snapshot._refs` values): opaque, valid only in this
/// process. The real walker stores an `ax_ffi::AxElement`; tests store anything.
#[derive(Clone)]
pub struct AxRef(pub Arc<dyn Any + Send + Sync>);

impl AxRef {
    pub fn new<T: Any + Send + Sync>(value: T) -> Self {
        AxRef(Arc::new(value))
    }

    pub fn downcast_ref<T: Any>(&self) -> Option<&T> {
        self.0.downcast_ref::<T>()
    }
}

impl std::fmt::Debug for AxRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AxRef({:p})", Arc::as_ptr(&self.0))
    }
}

/// Identity, as Python compares the objects (`is`).
impl PartialEq for AxRef {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// One snapshot of an app's focused window.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub seq: i64,
    pub app: String,
    pub bundle: String,
    pub title: String,
    pub pid: i64,
    /// reading order (y, then x)
    pub elements: Vec<Candidate>,
    /// ≤ 8: "title: …", "focused: …", static texts
    pub context_lines: Vec<String>,
    pub focused: Option<Candidate>,
    /// first static text of an open sheet/dialog, ≤ 80 chars
    pub dialog_text: String,
    pub fullscreen: bool,
    pub node_count: i64,
    pub truncated: bool,
    pub secs: f64,
    /// The live AXUIElement behind each candidate id, from the walk that made this snapshot (ax_snapshot with the
    /// real walker only; a fixture or a dict has none). Never serialised, never compared.
    pub refs: HashMap<String, AxRef>,
    /// Fresh, non-editable AXStaticText values whose descriptive label can differ from their displayed text
    /// (`_readouts`). Transport-only like refs: excluded from equality; decision_lane compares them explicitly.
    pub readouts: Vec<(String, String)>,
    /// Each control's raw href as the page collector read it (`_hrefs`), for the account-change rule. Transport-only:
    /// eve's browser fills it, the Mac walk and fixtures do not.
    pub hrefs: HashMap<String, String>,
}

/// The archived-schema equality of Python's frozen dataclass: refs, readouts and hrefs are `compare=False`.
impl PartialEq for Snapshot {
    fn eq(&self, other: &Self) -> bool {
        self.seq == other.seq
            && self.app == other.app
            && self.bundle == other.bundle
            && self.title == other.title
            && self.pid == other.pid
            && self.elements == other.elements
            && self.context_lines == other.context_lines
            && self.focused == other.focused
            && self.dialog_text == other.dialog_text
            && self.fullscreen == other.fullscreen
            && self.node_count == other.node_count
            && self.truncated == other.truncated
            && self.secs == other.secs
    }
}

impl Snapshot {
    /// Python `Snapshot.ref(cid)`.
    pub fn element_ref(&self, cid: &str) -> Option<&AxRef> {
        self.refs.get(cid)
    }

    /// The control's href: the collector's raw one, else an observed `link destination:` context line (a JSON
    /// object with "id" and "url"), else "".
    pub fn href(&self, cid: &str) -> String {
        if let Some(raw) = self.hrefs.get(cid) {
            if !raw.is_empty() {
                return raw.clone();
            }
        }
        for line in &self.context_lines {
            let Some(rest) = line.strip_prefix(DESTINATION_PREFIX) else {
                continue;
            };
            let Ok(d) = serde_json::from_str::<Value>(rest) else {
                continue;
            };
            if let Value::Object(m) = &d {
                let id = m.get("id").map_or_else(|| "None".to_string(), py_str);
                if id == cid {
                    if let Some(Value::String(url)) = m.get("url") {
                        return url.clone();
                    }
                }
            }
        }
        String::new()
    }

    /// Python `Snapshot.get(cid)` (a dict over the elements, so the last of a repeated id wins).
    pub fn get(&self, cid: &str) -> Option<&Candidate> {
        self.elements.iter().rev().find(|c| c.id == cid)
    }

    /// `{"seq", "app", "bundle", "title", "pid", "elements", "context_lines", "focused", "dialog_text",
    /// "fullscreen", "node_count", "truncated", "secs"}` in that order.
    pub fn to_dict(&self) -> Value {
        let mut m = Map::new();
        m.insert("seq".into(), Value::from(self.seq));
        m.insert("app".into(), Value::from(self.app.clone()));
        m.insert("bundle".into(), Value::from(self.bundle.clone()));
        m.insert("title".into(), Value::from(self.title.clone()));
        m.insert("pid".into(), Value::from(self.pid));
        m.insert("elements".into(), Value::Array(self.elements.iter().map(Candidate::to_dict).collect()));
        m.insert("context_lines".into(), Value::from(self.context_lines.clone()));
        m.insert("focused".into(), self.focused.as_ref().map_or(Value::Null, Candidate::to_dict));
        m.insert("dialog_text".into(), Value::from(self.dialog_text.clone()));
        m.insert("fullscreen".into(), Value::Bool(self.fullscreen));
        m.insert("node_count".into(), Value::from(self.node_count));
        m.insert("truncated".into(), Value::Bool(self.truncated));
        m.insert("secs".into(), f64_value(self.secs));
        Value::Object(m)
    }
}

/// Python `snapshot_from_dict`: the inverse of `Snapshot::to_dict` (Python's int()/float()/bool() coercions).
pub fn snapshot_from_dict(d: &Value) -> anyhow::Result<Snapshot> {
    let int_or = |key: &str, default: i64| -> anyhow::Result<i64> {
        match d.get(key) {
            None => Ok(default),
            Some(v) => py_int(v).ok_or_else(|| anyhow::anyhow!("ValueError: {key} is {}", py_str(v))),
        }
    };
    let str_or = |key: &str| d.get(key).map(py_str).unwrap_or_default();
    let flag = |key: &str| d.get(key).is_some_and(py_truthy);
    let elements = match d.get("elements") {
        None => Vec::new(),
        Some(Value::Array(items)) => items.iter().map(Candidate::from_dict).collect::<anyhow::Result<_>>()?,
        Some(other) => anyhow::bail!("TypeError: elements is a {}", py_type_name(other)),
    };
    let focused = match d.get("focused") {
        Some(f) if py_truthy(f) => Some(Candidate::from_dict(f)?),
        _ => None,
    };
    let secs = match d.get("secs") {
        None => 0.0,
        Some(v) => py_float(v).ok_or_else(|| anyhow::anyhow!("ValueError: secs is {}", py_str(v)))?,
    };
    Ok(Snapshot {
        seq: int_or("seq", 0)?,
        app: str_or("app"),
        bundle: str_or("bundle"),
        title: str_or("title"),
        pid: int_or("pid", 0)?,
        elements,
        context_lines: match d.get("context_lines") {
            Some(v) => str_items(v)?,
            None => Vec::new(),
        },
        focused,
        dialog_text: str_or("dialog_text"),
        fullscreen: flag("fullscreen"),
        node_count: int_or("node_count", 0)?,
        truncated: flag("truncated"),
        secs,
        ..Snapshot::default()
    })
}

pub fn is_pressable(c: &Candidate) -> bool {
    c.actions.iter().any(|a| a == "AXPress") || PRESSABLE_ROLES.contains(&c.role.as_str())
}

/// The labelled, enabled controls the lane could click (before the rank drops).
pub fn pressable(snapshot: &Snapshot) -> Vec<&Candidate> {
    snapshot.elements.iter().filter(|c| is_pressable(c) && !c.label.is_empty() && c.enabled).collect()
}

// ---- deriving a snapshot from a raw walk ---------------------------------------------------------

/// `_SPACES.sub(" ", text).strip()`.
fn collapse_spaces(text: &str) -> String {
    py_strip(&SPACES_RE.replace_all(text, " ")).to_string()
}

/// Python `_text`: "" for null and bools, `str()` of a number, else whitespace-collapsed text.
pub fn text(v: &Value) -> String {
    match v {
        Value::Null | Value::Bool(_) => String::new(),
        Value::Number(_) => py_str(v),
        Value::String(s) => collapse_spaces(s),
        other => collapse_spaces(&py_str(other)),
    }
}

/// `_text(node.get(key))`.
fn text_at(node: &Value, key: &str) -> String {
    node.get(key).map(text).unwrap_or_default()
}

/// Python `_role_name`.
pub fn role_name(role: &str, subrole: &str) -> String {
    if subrole == "AXTabButton" || role == "AXTab" {
        return "tab".into();
    }
    if subrole == "AXSearchField" {
        return "search field".into();
    }
    if subrole == "AXToggle" || subrole == "AXSwitch" {
        return "toggle".into();
    }
    if let Some((_, name)) = ROLE_NAMES.iter().find(|(r, _)| *r == role) {
        return (*name).into();
    }
    if role.starts_with("AX") && py_len(role) > 2 {
        return casefold(&role[2..]);
    }
    let folded = casefold(role);
    if folded.is_empty() {
        "element".into()
    } else {
        folded
    }
}

fn is_str(v: Option<&Value>, s: &str) -> bool {
    matches!(v, Some(Value::String(x)) if x == s)
}

/// Python `_descendant_text(node, depth=4)`: the first text under a node, in tree order (rows, cells, links and
/// web buttons keep their label in a child AXStaticText; a Finder list row keeps the file name in an AXTextField).
/// An image, button or link title is only the fallback: a Music sidebar row is [AXImage "clock", AXStaticText
/// "Recently Added"] and must read "Recently Added".
pub fn descendant_text(node: &Value, depth: usize) -> String {
    let mut stack: Vec<(&Value, usize)> = children_of(node).iter().rev().map(|c| (c, 1)).collect();
    let mut fallback = String::new();
    while let Some((n, d)) = stack.pop() {
        if is_str(n.get("role"), "AXSecureTextField") || is_str(n.get("subrole"), "AXSecureTextField") {
            continue;
        }
        let role = n.get("role");
        if is_str(role, "AXStaticText") {
            let mut t = text_at(n, "value");
            if t.is_empty() {
                t = text_at(n, "title");
            }
            if t.is_empty() {
                t = text_at(n, "description");
            }
            if !t.is_empty() {
                return t;
            }
        } else if is_str(role, "AXTextField") {
            let t = text_at(n, "value");
            if !t.is_empty() {
                return t;
            }
        } else if d < depth
            && fallback.is_empty()
            && (is_str(role, "AXImage") || is_str(role, "AXButton") || is_str(role, "AXLink"))
        {
            fallback = text_at(n, "title");
            if fallback.is_empty() {
                fallback = text_at(n, "description");
            }
        }
        if d < depth {
            stack.extend(children_of(n).iter().rev().map(|c| (c, d + 1)));
        }
    }
    fallback
}

/// `raw in (1, True, "1")`, with Python's numeric equality.
fn is_one(raw: &Value) -> bool {
    match raw {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() == Some(1.0),
        Value::String(s) => s == "1",
        _ => false,
    }
}

/// `raw in (0, False, "0")`.
fn is_zero(raw: &Value) -> bool {
    match raw {
        Value::Bool(b) => !*b,
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::String(s) => s == "0",
        _ => false,
    }
}

/// `text[:n - 1] + "…"` when longer than `n` characters.
fn cap(text: String, n: usize) -> String {
    if py_len(&text) > n {
        format!("{}…", py_take(&text, n - 1))
    } else {
        text
    }
}

/// Python `_value_text`.
pub fn value_text(role: &str, raw: &Value) -> String {
    match role {
        "radio button" | "tab" => (if is_one(raw) { "selected" } else { "" }).into(),
        "checkbox" | "toggle" => {
            if is_one(raw) {
                "checked".into()
            } else if is_zero(raw) {
                "unchecked".into()
            } else {
                String::new()
            }
        }
        "disclosure triangle" => (if is_one(raw) { "expanded" } else { "collapsed" }).into(),
        "static text" => String::new(),
        _ => cap(text(raw), MAX_VALUE_CHARS),
    }
}

/// Python `_frame`: four finite numbers rounded to ints with a positive size, else None.
pub fn frame(raw: &Value) -> Option<Rect> {
    let items = raw.as_array()?;
    if items.len() != 4 {
        return None;
    }
    let mut out = [0i64; 4];
    for (slot, v) in out.iter_mut().zip(items) {
        // System Settings reports an infinite frame on some rows
        let f = py_float(v)?;
        if !f.is_finite() {
            return None;
        }
        let r = f.round_ties_even();
        if r < i64::MIN as f64 || r > i64::MAX as f64 {
            return None;
        }
        *slot = r as i64;
    }
    let [x, y, w, h] = out;
    if w <= 0 || h <= 0 {
        return None;
    }
    Some((x, y, w, h))
}

/// Python `_on_screen`.
pub fn on_screen(frame: Rect, screen: Option<(i64, i64)>) -> bool {
    let Some((sw, sh)) = screen else {
        return true;
    };
    let (x, y, w, h) = frame;
    x + w > 0 && y + h > 0 && x < sw && y < sh
}

/// One found control before the reading-order sort.
struct Found {
    key: (i64, i64, usize),
    candidate: Candidate,
    focused: bool,
    path: NodePath,
    readout: String,
}

/// Derive the Snapshot from a raw walk dump. Pure and deterministic: the same dump gives an equal Snapshot every
/// time. `refs` is ax_walk's {node path: element} for this same dump; each candidate's element is kept in
/// `Snapshot::refs`.
pub fn snapshot_from_raw(
    raw: &Value,
    seq: i64,
    screen: Option<(i64, i64)>,
    refs: Option<&HashMap<NodePath, AxRef>>,
) -> Snapshot {
    let app = text_at(raw, "app");
    let bundle = text_at(raw, "bundle");
    let int_field = |key: &str| raw.get(key).filter(|v| py_truthy(v)).and_then(py_int).unwrap_or(0);
    let pid = int_field("pid");
    let root = raw.get("root").filter(|r| py_truthy(r));
    let mut title = text_at(raw, "title");
    if title.is_empty() {
        title = root.map(|r| text_at(r, "title")).unwrap_or_default();
    }
    let secs = raw.get("secs").filter(|v| py_truthy(v)).and_then(py_float).unwrap_or(0.0);
    let base = Snapshot {
        seq,
        app: app.clone(),
        bundle,
        title: title.clone(),
        pid,
        fullscreen: raw.get("fullscreen").is_some_and(py_truthy),
        node_count: int_field("node_count"),
        truncated: raw.get("truncated").is_some_and(py_truthy),
        secs: round4(secs),
        ..Snapshot::default()
    };
    let Some(root) = root else {
        let lines = if title.is_empty() { Vec::new() } else { vec![format!("title: {title}")] };
        return Snapshot { context_lines: lines, ..base };
    };
    let window_dialog = DIALOG_SUBROLES.contains(&text_at(root, "subrole").as_str());
    let mut found: Vec<Found> = Vec::new();
    let mut dialog_text = String::new();
    // (node, ancestors, row_label, secure_parent, path)
    let mut stack: Vec<(&Value, Vec<String>, String, bool, NodePath)> =
        vec![(root, Vec::new(), String::new(), false, Vec::new())];
    let mut order = 0usize;
    while let Some((node, ancestors, row_label, secure_parent, path)) = stack.pop() {
        let ax_role = text_at(node, "role");
        let subrole = text_at(node, "subrole");
        let role = role_name(&ax_role, &subrole);
        let actions: Vec<String> = match node.get("actions") {
            Some(v) if py_truthy(v) => str_items(v).unwrap_or_default(),
            _ => Vec::new(),
        };
        let in_sheet = ancestors.iter().any(|a| a == "AXSheet") || ax_role == "AXSheet";
        let in_content = ancestors.iter().any(|a| CONTENT_ROLES.contains(&a.as_str()));
        let mut label = text_at(node, "title");
        if label.is_empty() {
            label = text_at(node, "description");
        }
        if label.is_empty() && role == "static text" {
            label = text_at(node, "value");
        }
        if label.is_empty() && ax_role != "AXWindow" && !CONTAINER_ROLES.contains(&ax_role.as_str()) {
            label = descendant_text(node, 4);
        }
        if role == "static text" && !label.is_empty() && (in_sheet || window_dialog) && dialog_text.is_empty() {
            dialog_text = py_take(&label, MAX_DIALOG_CHARS);
        }
        let null = Value::Null;
        let raw_value = node.get("value").unwrap_or(&null);
        let rect = frame(node.get("frame").unwrap_or(&null));
        let value = value_text(&role, raw_value);
        let editable = EDITABLE_ROLES.contains(&role.as_str());
        let secure = secure_parent || ax_role == "AXSecureTextField" || subrole == "AXSecureTextField";
        // A cell that only repeats its row's label is the same control twice (System Settings
        // sidebar: 39 AXRow/AXCell pairs); the row keeps the slot.
        let repeats_row = role == "cell" && !label.is_empty() && label == row_label;
        let is_candidate = ax_role != "AXWindow"
            && rect.is_some_and(|r| on_screen(r, screen))
            && (!label.is_empty() || !value.is_empty() || editable)
            && !repeats_row
            && (ROLE_NAMES.iter().any(|(_, name)| *name == role) || actions.iter().any(|a| a == "AXPress"));
        if let (true, Some(rect)) = (is_candidate, rect) {
            // Static text nested in an editable field can be its user's draft, not a
            // separate result display. It must not become a new model value channel.
            let under_editable = ancestors
                .iter()
                .any(|a| matches!(a.as_str(), "AXTextField" | "AXSearchField" | "AXTextArea" | "AXComboBox"));
            let readout = if role == "static text" && !secure && !under_editable {
                value_text("readout", raw_value)
            } else {
                String::new()
            };
            found.push(Found {
                key: (rect.1, rect.0, order),
                candidate: Candidate {
                    id: String::new(),
                    role: role.clone(),
                    label: label.clone(),
                    value,
                    frame: rect,
                    actions,
                    enabled: node.get("enabled").is_none_or(py_truthy),
                    app: app.clone(),
                    in_content,
                    in_dialog: window_dialog || in_sheet,
                    secure,
                    editable,
                },
                focused: node.get("focused").is_some_and(py_truthy),
                path: path.clone(),
                readout,
            });
            order += 1;
        }
        if SKIP_DESCEND_ROLES.contains(&ax_role.as_str()) {
            continue;
        }
        let mut below = ancestors;
        below.push(ax_role);
        let inner_row = if role == "row" { label } else { row_label };
        for (i, child) in children_of(node).iter().enumerate().rev() {
            let mut child_path = path.clone();
            child_path.push(i);
            stack.push((child, below.clone(), inner_row.clone(), secure, child_path));
        }
    }
    found.sort_by_key(|f| f.key);
    let mut elements: Vec<Candidate> = Vec::with_capacity(found.len());
    let mut focused: Option<Candidate> = None;
    let mut kept_refs: HashMap<String, AxRef> = HashMap::new();
    let mut readouts: Vec<(String, String)> = Vec::new();
    for (index, f) in found.into_iter().enumerate() {
        let mut c = f.candidate;
        c.id = index.to_string();
        if let Some(element) = refs.and_then(|r| r.get(&f.path)) {
            kept_refs.insert(c.id.clone(), element.clone());
        }
        if !f.readout.is_empty() {
            readouts.push((c.id.clone(), f.readout));
        }
        if f.focused && focused.is_none() {
            focused = Some(c.clone());
        }
        elements.push(c);
    }
    let mut lines: Vec<String> = Vec::new();
    if !title.is_empty() {
        lines.push(format!("title: {title}"));
    }
    if let Some(fc) = focused.as_ref().filter(|fc| !fc.secure) {
        lines.push(py_rstrip(&format!("focused: {} {}", fc.role, fc.label)).to_string());
    }
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    if !title.is_empty() {
        seen.insert(casefold(&title));
    }
    for c in &elements {
        if lines.len() >= MAX_CONTEXT_LINES || lines.len() >= MAX_CONTEXT_TEXTS + 2 {
            break;
        }
        if c.secure || c.role != "static text" || c.label.is_empty() || py_len(&c.label) > 60 {
            continue;
        }
        let folded = casefold(&c.label);
        if seen.contains(&folded) {
            continue;
        }
        seen.insert(folded);
        lines.push(c.label.clone());
    }
    lines.truncate(MAX_CONTEXT_LINES);
    Snapshot { elements, context_lines: lines, focused, dialog_text, refs: kept_refs, readouts, ..base }
}

/// The injectable walk backend: `walk(pid, max_nodes, budget_secs)` → the raw dump.
pub type WalkFn<'a> = &'a dyn Fn(i64, usize, f64) -> Value;

/// One snapshot of an app's focused window (the frontmost app — desktop_helpers::focused_pid — when pid is None).
/// `walk` is the injectable backend; the default is the real AX walker, whose live elements are kept. Each call
/// gets a fresh `seq`.
pub fn ax_snapshot(
    pid: Option<i64>,
    max_nodes: usize,
    budget_secs: f64,
    walk: Option<WalkFn<'_>>,
    screen: Option<(i64, i64)>,
) -> Snapshot {
    let pid = pid.unwrap_or_else(crate::desktop_helpers::focused_pid);
    match walk {
        None => {
            let mut refs: HashMap<NodePath, AxRef> = HashMap::new();
            let raw = ax_walk(pid, max_nodes, budget_secs, CONTENT_FIRST, &crate::perf_counter, Some(&mut refs));
            snapshot_from_raw(&raw, next_seq(), screen, Some(&refs))
        }
        Some(walk) => {
            let raw = walk(pid, max_nodes, budget_secs);
            snapshot_from_raw(&raw, next_seq(), screen, None)
        }
    }
}

// ---- the menu --------------------------------------------------------------------------------------

static TOKEN_RE: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"[a-z0-9]+").expect("token"));

/// Casefolded words of ≥ 3 characters minus stop-words, in order, without duplicates.
pub fn objective_tokens(objective: &str) -> Vec<String> {
    let folded = casefold(objective);
    let mut out: Vec<String> = Vec::new();
    for m in TOKEN_RE.find_iter(&folded) {
        let tok = m.as_str();
        if tok.len() >= 3 && !STOP_WORDS.contains(&tok) && !out.iter().any(|t| t == tok) {
            out.push(tok.to_string());
        }
    }
    out
}

pub fn score_candidate(c: &Candidate, tokens: &[String]) -> i64 {
    let hay = casefold(&format!("{} {}", c.label, c.value));
    let prior = ROLE_PRIOR.iter().find(|(r, _)| *r == c.role).map_or(0, |(_, p)| *p);
    prior + 4 * tokens.iter().filter(|t| hay.contains(t.as_str())).count() as i64
}

/// `kinds=` of rank_candidates. Python raises ValueError for any other string; the enum makes that impossible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kinds {
    Pressable,
    Editable,
}

/// (kept, dropped_by_cap). Drops before scoring: disabled, empty label, in_dialog, secure, in_content links,
/// images and static text unless allow_page_links, needs_confirmation(c, its href) unless allow_sensitive.
/// Stable by score, then reading order. `max_out` below zero is zero.
pub fn rank_candidates(
    snapshot: &Snapshot,
    objective: &str,
    max_out: i64,
    kinds: Kinds,
    allow_page_links: bool,
    allow_sensitive: bool,
) -> (Vec<Candidate>, usize) {
    let tokens = objective_tokens(objective);
    let mut pool: Vec<&Candidate> = Vec::new();
    for c in &snapshot.elements {
        if kinds == Kinds::Pressable && !is_pressable(c) {
            continue;
        }
        if kinds == Kinds::Editable && !c.editable {
            continue;
        }
        if !c.enabled || py_strip(&c.label).is_empty() || c.in_dialog || c.secure {
            continue;
        }
        if c.in_content && !allow_page_links && matches!(c.role.as_str(), "link" | "image" | "static text") {
            continue;
        }
        if !allow_sensitive && needs_confirmation(c, &snapshot.href(&c.id)) {
            continue;
        }
        pool.push(c);
    }
    // stable: reading order breaks ties
    pool.sort_by_key(|c| -score_candidate(c, &tokens));
    let cap = usize::try_from(max_out.max(0)).unwrap_or(usize::MAX);
    let dropped = pool.len().saturating_sub(cap);
    (pool.into_iter().take(cap).cloned().collect(), dropped)
}

/// `text[:max(1, max_chars - 1)].rstrip() + "…"` when longer than `max_chars` characters.
fn cap_rstrip(text: String, max_chars: usize) -> String {
    if py_len(&text) > max_chars {
        format!("{}…", py_rstrip(&py_take(&text, max_chars.saturating_sub(1).max(1))))
    } else {
        text
    }
}

/// "click radio button: Week (selected)" — URLs stripped, whitespace collapsed, label capped at `max_chars`
/// (Python default 40).
pub fn describe(c: &Candidate, verb: &str, max_chars: usize) -> String {
    let label = cap_rstrip(collapse_spaces(&URL_RE.replace_all(&c.label, "")), max_chars);
    let mut text = format!("{verb} {}: {label}", c.role).trim_end_matches([':', ' ']).to_string();
    if !c.value.is_empty() {
        let value = if py_len(&c.value) <= 24 { c.value.clone() } else { format!("{}…", py_take(&c.value, 23)) };
        text.push_str(&format!(" ({value})"));
    }
    text
}

/// One line for the decider's state: app, title, focused control, a few static texts (Python default 300).
pub fn context_summary(snapshot: &Snapshot, max_chars: usize) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !snapshot.app.is_empty() {
        parts.push(format!("app: {}", snapshot.app));
    }
    let mut texts: Vec<&str> = Vec::new();
    for line in &snapshot.context_lines {
        if line.starts_with("title: ") || line.starts_with("focused: ") {
            parts.push(line.clone());
        } else {
            texts.push(line);
        }
    }
    if !snapshot.dialog_text.is_empty() {
        parts.push(format!("dialog: {}", snapshot.dialog_text));
    }
    if !texts.is_empty() {
        parts.push(format!("text: {}", texts.join(" | ")));
    }
    cap_rstrip(parts.join("; "), max_chars)
}

// ---- the real walker (macOS) -----------------------------------------------------------------------

/// Python `_app_identity`: (localizedName, bundleIdentifier) of a pid, ("", "") when unknown or off the Mac.
pub fn app_identity(pid: i64) -> (String, String) {
    #[cfg(target_os = "macos")]
    {
        // a nameless app is still walkable
        if let Ok(pid) = i32::try_from(pid) {
            if let Some(identity) = crate::ax_ffi::running_app(pid) {
                return identity;
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = pid;
    (String::new(), String::new())
}

/// The pid of a running app by its localized name (case-insensitive), 0 when not running (or off the Mac).
pub fn pid_for_app(name: &str) -> i64 {
    #[cfg(target_os = "macos")]
    {
        let wanted = casefold(name);
        for (app_name, pid) in crate::ax_ffi::running_apps() {
            if casefold(&app_name) == wanted {
                return i64::from(pid);
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = name;
    0
}

/// The main display's size in points; None when there is no display (no viewport filter).
pub fn main_screen_points() -> Option<(i64, i64)> {
    #[cfg(target_os = "macos")]
    {
        let bounds = objc2_core_graphics::CGDisplayBounds(objc2_core_graphics::CGMainDisplayID());
        Some((bounds.size.width as i64, bounds.size.height as i64))
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// An attribute value after the walker's `scalar()`: an AXError, range, rect or unknown value is `Null`; lists keep
/// their elements (AXChildren).
#[derive(Debug, Clone)]
pub enum AxScalar<E> {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Point(f64, f64),
    Size(f64, f64),
    List(Vec<E>),
}

impl<E> AxScalar<E> {
    /// Python's `bool(v)`.
    fn truthy(&self) -> bool {
        match self {
            AxScalar::Null => false,
            AxScalar::Bool(b) => *b,
            AxScalar::Int(i) => *i != 0,
            AxScalar::Float(f) => *f != 0.0,
            AxScalar::Str(s) => !s.is_empty(),
            AxScalar::Point(..) | AxScalar::Size(..) => true,
            AxScalar::List(items) => !items.is_empty(),
        }
    }

    /// Python's `str(v or "")`.
    fn str_or_empty(&self) -> String {
        if !self.truthy() {
            return String::new();
        }
        let float = |f: f64| eve_decider::py_repr(&f64_value(f));
        match self {
            AxScalar::Null => String::new(),
            AxScalar::Bool(b) => py_bool(*b).into(),
            AxScalar::Int(i) => i.to_string(),
            AxScalar::Float(f) => float(*f),
            AxScalar::Str(s) => s.clone(),
            AxScalar::Point(a, b) | AxScalar::Size(a, b) => format!("({}, {})", float(*a), float(*b)),
            AxScalar::List(items) => format!("[{}]", vec!["<AXUIElement>"; items.len()].join(", ")),
        }
    }

    /// `(x, y)` when the value is a point or a size (both are tuples to the Python).
    fn pair(&self) -> Option<(f64, f64)> {
        match self {
            AxScalar::Point(a, b) | AxScalar::Size(a, b) => Some((*a, *b)),
            _ => None,
        }
    }
}

/// What ax_walk needs from Accessibility: the seam the tests drive with a fake tree and the Mac fills with AX IPC.
pub trait AxWalkBackend {
    type Element;
    /// Python `_app_identity(pid)`.
    fn app_identity(&self, pid: i64) -> (String, String);
    /// AXFocusedWindow, else the first of AXWindows; None when the app has no window.
    fn window(&self, pid: i64) -> Option<Self::Element>;
    /// AXFullScreen of the window when it reads as a bool or an int, else false.
    fn fullscreen(&self, window: &Self::Element) -> bool;
    /// The ATTRIBUTES in one call, each through `scalar()`; None when the call fails or comes back short.
    fn values(&self, el: &Self::Element) -> Option<Vec<AxScalar<Self::Element>>>;
    /// AXUIElementCopyActionNames; None on an error or no names.
    fn action_names(&self, el: &Self::Element) -> Option<Vec<String>>;
    /// The element as a snapshot ref.
    fn element_ref(&self, el: &Self::Element) -> AxRef;
}

/// The real backend: AX IPC through `ax_ffi`.
#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, Default)]
pub struct MacAx;

#[cfg(target_os = "macos")]
impl MacAx {
    /// The walker's `scalar()` over one converted attribute.
    fn scalar(attr: crate::ax_ffi::AxAttr) -> AxScalar<crate::ax_ffi::AxElement> {
        use crate::ax_ffi::AxAttr;
        match attr {
            AxAttr::Null | AxAttr::Element(_) => AxScalar::Null,
            AxAttr::Bool(b) => AxScalar::Bool(b),
            AxAttr::Int(i) => AxScalar::Int(i),
            AxAttr::Float(f) => AxScalar::Float(f),
            AxAttr::Str(s) => AxScalar::Str(s),
            AxAttr::Point(x, y) => AxScalar::Point(x, y),
            AxAttr::Size(w, h) => AxScalar::Size(w, h),
            AxAttr::Array(items) => AxScalar::List(
                items
                    .into_iter()
                    .filter_map(|item| match item {
                        AxAttr::Element(el) => Some(el),
                        _ => None,
                    })
                    .collect(),
            ),
        }
    }
}

#[cfg(target_os = "macos")]
impl AxWalkBackend for MacAx {
    type Element = crate::ax_ffi::AxElement;

    fn app_identity(&self, pid: i64) -> (String, String) {
        app_identity(pid)
    }

    fn window(&self, pid: i64) -> Option<Self::Element> {
        use crate::ax_ffi::{AxAttr, AxElement};
        let pid = i32::try_from(pid).ok()?;
        let app = AxElement::application(pid);
        if let Ok(AxAttr::Element(window)) = app.attribute("AXFocusedWindow") {
            return Some(window);
        }
        match app.attribute("AXWindows") {
            Ok(AxAttr::Array(windows)) => match windows.into_iter().next() {
                Some(AxAttr::Element(window)) => Some(window),
                _ => None,
            },
            _ => None,
        }
    }

    fn fullscreen(&self, window: &Self::Element) -> bool {
        use crate::ax_ffi::AxAttr;
        match window.attribute("AXFullScreen") {
            Ok(AxAttr::Bool(b)) => b,
            Ok(AxAttr::Int(i)) => i != 0,
            _ => false,
        }
    }

    fn values(&self, el: &Self::Element) -> Option<Vec<AxScalar<Self::Element>>> {
        let values = el.attributes(&ATTRIBUTES).ok()?;
        if values.len() != ATTRIBUTES.len() {
            return None;
        }
        Some(values.into_iter().map(Self::scalar).collect())
    }

    fn action_names(&self, el: &Self::Element) -> Option<Vec<String>> {
        el.action_names().ok().filter(|names| !names.is_empty())
    }

    fn element_ref(&self, el: &Self::Element) -> AxRef {
        AxRef::new(el.clone())
    }
}

/// Off the Mac there is no Accessibility: every app reads as windowless.
#[cfg(not(target_os = "macos"))]
struct NoAx;

#[cfg(not(target_os = "macos"))]
impl AxWalkBackend for NoAx {
    type Element = ();

    fn app_identity(&self, pid: i64) -> (String, String) {
        app_identity(pid)
    }

    fn window(&self, _pid: i64) -> Option<()> {
        None
    }

    fn fullscreen(&self, _window: &()) -> bool {
        false
    }

    fn values(&self, _el: &()) -> Option<Vec<AxScalar<()>>> {
        None
    }

    fn action_names(&self, _el: &()) -> Option<Vec<String>> {
        None
    }

    fn element_ref(&self, _el: &()) -> AxRef {
        AxRef::new(())
    }
}

/// One fetched node of the walk: its dump fields (children and ancestor_roles go in at assembly), the children it
/// reported, and its live element when refs are wanted.
struct WalkNode<E> {
    fields: Map<String, Value>,
    role: String,
    ancestors: Vec<String>,
    children: Vec<usize>,
    element: Option<AxRef>,
    _kind: std::marker::PhantomData<E>,
}

/// A fetched node and the children it reported.
type Fetched<E> = (WalkNode<E>, Vec<E>);

/// The raw accessibility tree of an app's focused window through `backend` (shape in the module docs).
///
/// `refs`, when given, is filled with {node path: element} for every node of the dump, so snapshot_from_raw can
/// keep each candidate's live element. `content_first` orders siblings so AXWebArea / AXScrollArea /
/// AXContentList subtrees are walked before the window chrome; false walks chrome first.
pub fn ax_walk_with<B: AxWalkBackend>(
    backend: &B,
    pid: i64,
    max_nodes: usize,
    budget_secs: f64,
    content_first: bool,
    clock: crate::Clock<'_>,
    refs: Option<&mut HashMap<NodePath, AxRef>>,
) -> Value {
    let t0 = clock();
    let (name, bundle) = backend.app_identity(pid);
    let mut raw = Map::new();
    raw.insert("app".into(), Value::from(name));
    raw.insert("bundle".into(), Value::from(bundle));
    raw.insert("pid".into(), Value::from(pid));
    raw.insert("title".into(), Value::from(""));
    raw.insert("fullscreen".into(), Value::Bool(false));
    raw.insert("node_count".into(), Value::from(0));
    raw.insert("truncated".into(), Value::Bool(false));
    raw.insert("secs".into(), f64_value(0.0));
    raw.insert("root".into(), Value::Null);
    let finish = |mut raw: Map<String, Value>| {
        raw.insert("secs".into(), f64_value(round4(clock() - t0)));
        Value::Object(raw)
    };
    if pid == 0 {
        return finish(raw);
    }
    let Some(window) = backend.window(pid) else {
        return finish(raw);
    };
    let want_refs = refs.is_some();

    let fetch = |el: &B::Element, ancestors: Vec<String>| -> Option<Fetched<B::Element>> {
        let values = backend.values(el)?;
        let Ok::<[AxScalar<B::Element>; 10], _>(
            [role, subrole, title, desc, value, pos, size, enabled, focused, children],
        ) = values.try_into()
        else {
            return None;
        };
        let role_s = role.str_or_empty();
        let subrole_s = subrole.str_or_empty();
        let frame = match (pos.pair(), size.pair()) {
            (Some((x, y)), Some((w, h))) => Value::Array(vec![f64_value(x), f64_value(y), f64_value(w), f64_value(h)]),
            _ => Value::Null,
        };
        let text_value = match &value {
            AxScalar::Str(s) => Value::from(py_take(s, 200)),
            AxScalar::Bool(b) => Value::Bool(*b),
            AxScalar::Int(i) => Value::from(*i),
            AxScalar::Float(f) => f64_value(*f),
            _ => Value::Null,
        };
        let mut actions: Vec<String> = Vec::new();
        // One extra IPC only where it can change the answer: a node whose role does not already
        // make it pressable (Calendar events are AXStaticText with AXPress), outside page content
        // (web action queries cost ~0.45 ms a node and page text is never offered) and not a container.
        if !CONTAINER_ROLES.contains(&role_s.as_str())
            && !PRESSABLE_ROLES.contains(&role_name(&role_s, &subrole_s).as_str())
            && !ancestors.iter().any(|a| CONTENT_ROLES.contains(&a.as_str()))
        {
            if let Some(names) = backend.action_names(el) {
                actions = names;
            }
        }
        let mut fields = Map::new();
        fields.insert("role".into(), Value::from(role_s.clone()));
        fields.insert("subrole".into(), Value::from(subrole_s));
        fields.insert("title".into(), Value::from(py_take(&title.str_or_empty(), 200)));
        fields.insert("description".into(), Value::from(py_take(&desc.str_or_empty(), 200)));
        fields.insert("value".into(), text_value);
        fields.insert("frame".into(), frame);
        fields.insert("actions".into(), Value::from(actions));
        fields.insert("enabled".into(), Value::Bool(matches!(enabled, AxScalar::Null) || enabled.truthy()));
        fields.insert("focused".into(), Value::Bool(focused.truthy()));
        let kids = match children {
            AxScalar::List(kids) => kids,
            _ => Vec::new(),
        };
        let node = WalkNode {
            fields,
            role: role_s,
            ancestors,
            children: Vec::new(),
            element: want_refs.then(|| backend.element_ref(el)),
            _kind: std::marker::PhantomData,
        };
        Some((node, kids))
    };

    let Some((root, root_kids)) = fetch(&window, Vec::new()) else {
        return finish(raw);
    };
    let mut count: usize = 1;
    let mut truncated = false;
    raw.insert("fullscreen".into(), Value::Bool(backend.fullscreen(&window)));
    raw.insert("title".into(), root.fields.get("title").cloned().unwrap_or_else(|| Value::from("")));
    let mut arena: Vec<WalkNode<B::Element>> = vec![root];
    let mut stack: Vec<(usize, Vec<B::Element>)> = vec![(0, root_kids)];
    while !truncated {
        let Some((idx, kids)) = stack.pop() else {
            break;
        };
        if SKIP_DESCEND_ROLES.contains(&arena[idx].role.as_str()) {
            continue;
        }
        let mut below = arena[idx].ancestors.clone();
        below.push(arena[idx].role.clone());
        let mut fetched: Vec<Fetched<B::Element>> = Vec::new();
        for kid in &kids {
            if count >= max_nodes || clock() - t0 >= budget_secs {
                truncated = true;
                break;
            }
            let rec = fetch(kid, below.clone());
            count += 1;
            if let Some(rec) = rec {
                fetched.push(rec);
            }
        }
        // Sibling order decides what a truncated walk keeps: content subtrees first (the plan's
        // default, the page survives) or chrome first (the toolbar survives).
        fetched.sort_by_key(|(n, _)| usize::from(CONTENT_FIRST_ROLES.contains(&n.role.as_str()) != content_first));
        let mut pushed: Vec<(usize, Vec<B::Element>)> = Vec::with_capacity(fetched.len());
        for (node, node_kids) in fetched {
            let new_idx = arena.len();
            arena.push(node);
            arena[idx].children.push(new_idx);
            pushed.push((new_idx, node_kids));
        }
        stack.extend(pushed.into_iter().rev());
    }

    // Paths top-down (a child is always created after its parent), then the nested dump bottom-up.
    if let Some(refs) = refs {
        let mut paths: Vec<NodePath> = vec![Vec::new(); arena.len()];
        for idx in 0..arena.len() {
            for (i, &child) in arena[idx].children.iter().enumerate() {
                let mut p = paths[idx].clone();
                p.push(i);
                paths[child] = p;
            }
            if let Some(element) = arena[idx].element.clone() {
                refs.insert(paths[idx].clone(), element);
            }
        }
    }
    let mut built: Vec<Option<Value>> = vec![None; arena.len()];
    for idx in (0..arena.len()).rev() {
        let node = &mut arena[idx];
        let mut fields = std::mem::take(&mut node.fields);
        let children: Vec<Value> = node.children.iter().map(|&c| built[c].take().unwrap_or(Value::Null)).collect();
        fields.insert("children".into(), Value::Array(children));
        fields.insert("ancestor_roles".into(), Value::from(node.ancestors.clone()));
        built[idx] = Some(Value::Object(fields));
    }
    raw.insert("root".into(), built[0].take().unwrap_or(Value::Null));
    raw.insert("node_count".into(), Value::from(count));
    raw.insert("truncated".into(), Value::Bool(truncated));
    finish(raw)
}

/// Python `ax_walk(pid, ...)` with the real backend. Off the Mac: the empty dump Python returns for pid 0 (no
/// window).
pub fn ax_walk(
    pid: i64,
    max_nodes: usize,
    budget_secs: f64,
    content_first: bool,
    clock: crate::Clock<'_>,
    refs: Option<&mut HashMap<NodePath, AxRef>>,
) -> Value {
    #[cfg(target_os = "macos")]
    {
        ax_walk_with(&MacAx, pid, max_nodes, budget_secs, content_first, clock, refs)
    }
    #[cfg(not(target_os = "macos"))]
    {
        ax_walk_with(&NoAx, pid, max_nodes, budget_secs, content_first, clock, refs)
    }
}

// ---- CLI: capture a raw walk for the fixtures ------------------------------------------------------

#[derive(clap::Parser, Debug)]
#[command(
    name = "python -m eve_bridge.ax_candidates",
    about = "Walk one app's focused window and print its candidates."
)]
struct Cli {
    /// the app's name as the menu bar shows it (default: the frontmost app)
    #[arg(long)]
    app: Option<String>,
    /// the app's pid (overrides --app)
    #[arg(long)]
    pid: Option<i64>,
    #[arg(long = "max-nodes", default_value_t = DEFAULT_MAX_NODES)]
    max_nodes: usize,
    #[arg(long = "budget-secs", default_value_t = DEFAULT_BUDGET_SECS)]
    budget_secs: f64,
    /// rank the menu for this objective (table output)
    #[arg(long, default_value = "")]
    objective: String,
    /// print {raw, snapshot, candidates} as JSON
    #[arg(long)]
    json: bool,
}

/// `python -m eve_bridge.ax_candidates [--app NAME] [--pid PID] [--max-nodes N] [--budget-secs S] [--objective
/// TEXT] [--json]`: walk one app's focused window and print its candidates. `argv` excludes the program name.
/// Returns the exit code.
pub fn main(argv: &[String]) -> i32 {
    use clap::Parser;
    let args = match Cli::try_parse_from(std::iter::once("ax_candidates".to_string()).chain(argv.iter().cloned())) {
        Ok(args) => args,
        Err(e) => {
            let _ = e.print();
            return e.exit_code();
        }
    };
    let pid = match (args.pid, &args.app) {
        (Some(pid), _) if pid != 0 => pid,
        (_, Some(app)) if !app.is_empty() => {
            let pid = pid_for_app(app);
            if pid == 0 {
                eprintln!("no running app called {}", eve_decider::py_str_repr(app));
                return 2;
            }
            pid
        }
        _ => crate::desktop_helpers::focused_pid(),
    };
    let screen = main_screen_points();
    let raw = ax_walk(pid, args.max_nodes, args.budget_secs, CONTENT_FIRST, &crate::perf_counter, None);
    let snap = snapshot_from_raw(&raw, 1, screen, None);
    let (kept, dropped) = rank_candidates(&snap, &args.objective, 12, Kinds::Pressable, false, false);
    if args.json {
        let mut out = Map::new();
        out.insert("raw".into(), raw);
        out.insert("snapshot".into(), snap.to_dict());
        out.insert("screen".into(), screen.map_or(Value::Null, |(w, h)| Value::from(vec![w, h])));
        out.insert("candidates".into(), Value::from(kept.iter().map(|c| describe(c, "click", 40)).collect::<Vec<_>>()));
        println!("{}", eve_decisions::py_dumps(&Value::Object(out)));
        return 0;
    }
    println!("{} ({}) pid {} — {}", snap.app, snap.bundle, snap.pid, eve_decider::py_str_repr(&snap.title));
    println!(
        "nodes {}  elements {}  pressable {}  truncated {}  {:.0} ms  fullscreen {}",
        snap.node_count,
        snap.elements.len(),
        pressable(&snap).len(),
        py_bool(snap.truncated),
        snap.secs * 1000.0,
        py_bool(snap.fullscreen)
    );
    println!("context: {}", context_summary(&snap, 300));
    println!(
        "menu for {} ({} shown, {} dropped by cap):",
        eve_decider::py_str_repr(&args.objective),
        kept.len(),
        dropped
    );
    for c in &kept {
        let flags: String = [
            ("C", c.in_content),
            ("D", c.in_dialog),
            ("S", c.secure),
            ("E", c.editable),
            ("!", is_sensitive(&c.label)),
        ]
        .iter()
        .filter(|(_, on)| *on)
        .map(|(f, _)| *f)
        .collect();
        let (x, y, w, h) = c.frame;
        println!("  {:>4}  {:<52} ({x}, {y}, {w}, {h}) {flags}", c.id, describe(c, "click", 40));
    }
    0
}
