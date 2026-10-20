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
    "AXRole", "AXSubrole", "AXTitle", "AXDescription", "AXValue", "AXPosition", "AXSize", "AXEnabled", "AXFocused",
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
    "button", "link", "checkbox", "radio button", "menu item", "menu button", "pop up button", "tab", "toggle",
    "disclosure triangle", "row", "cell",
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
    "AXWindow", "AXGroup", "AXScrollArea", "AXSplitGroup", "AXToolbar", "AXList", "AXOutline", "AXTable",
    "AXWebArea", "AXLayoutArea", "AXLayoutItem", "AXTabGroup", "AXSplitter", "AXScrollBar", "AXBrowser", "AXDrawer",
    "AXSheet", "AXUnknown", "AXContentList", "AXGrowArea", "AXRulerMarker", "AXRuler", "AXMenuBar",
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
pub static URL_RE: LazyLock<regex::Regex> = LazyLock::new(|| todo!("ax_candidates: _URL"));
/// Python `_SPACES`: `\s+`.
pub static SPACES_RE: LazyLock<regex::Regex> = LazyLock::new(|| todo!("ax_candidates: _SPACES"));

/// The consequential verbs and phrases, as Python regex fragments, verbatim (`_SENSITIVE_PHRASES`).
pub const SENSITIVE_PHRASES: &[&str] = &[
    "delete", "remove", "trash", "erase", "empty", "discard", "don[’'`]?t\\s+save", "replace", "overwrite",
    "send", "submit", "post", "reply", "pay", "buy", "purchase", "subscribe", "checkout", "check\\s+out",
    "sign\\s+in", "signin", "log\\s+in", "login", "log\\s+out", "logout", "sign\\s+out", "signout", "password",
    "authori[sz]e", "permissions?", "allow", "captcha", "upload", "share", "export", "install", "uninstall",
    "quit", "close\\s+(?:window|tab|all)", "shut\\s+down", "shutdown", "restart", "reset", "forget",
    "disconnect", "revoke", "eject", "call", "facetime", "decline", "accept", "agree", "continue", "ok",
    "okay", "yes", "apply", "archive", "unsubscribe", "block", "report", "move\\s+to\\s+trash",
    "system\\s+settings", "security",
    // 2026-09-21: the verbs the request-level gates already knew (task_router.CONSEQUENTIAL, the risky_action
    // noul) and this table did not. They were masked while the lane only pressed a control the human had named
    // word for word; a plan executor presses controls nobody named. "Forward" (the browser's and Finder's
    // navigation button) and "Format" (Notes' toolbar) are left out on purpose: whole phrases cover the risky use.
    "place\\s+(?:an?\\s+|your\\s+|my\\s+)?order", "order\\s+now", "complete\\s+(?:order|purchase)", "confirm",
    "book", "publish", "transfer", "clear\\s+(?:history|all|data|cache|browsing\\s+data)",
    "cancel\\s+(?:subscription|membership|order|plan)", "merge\\s+pull\\s+request", "squash\\s+and\\s+merge",
    "rebase\\s+and\\s+merge", "format\\s+(?:disk|drive|volume)", "turn\\s+o(?:n|ff)", "approve", "kill",
    "add\\s+to\\s+(?:cart|bag|basket)", "mark\\s+as\\s+(?:spam|junk)", "spam", "junk", "revert",
    "sign\\s+up", "signup", "create\\s+(?:an?\\s+)?account", "donate", "withdraw", "deposit", "bid",
];
/// Word-bounded on both sides so "Postcode", "Composer", "Dispatcher", "Okay dokey"-style prefixes never fire on a
/// fragment, while "Delete Event", "Share…", "OK" and "Don't Save" always do. Python:
/// `(?<![a-z0-9])(?:<phrases>)(?![a-z0-9])`, IGNORECASE.
pub static SENSITIVE_LABEL: LazyLock<fancy_regex::Regex> =
    LazyLock::new(|| todo!("ax_candidates: SENSITIVE_LABEL"));

// Account-changing page controls: plain-looking links that vote, follow, star, hide or save as the signed-in
// user. The verb set and the short-label / endpoint split are adapted from LayaBrowse (src/laya_voice_browser/
// safety.py, MIT, Copyright (c) 2026 Aryan Bhujade). The first-word anchor, the chrome nouns and the last-segment
// endpoint match are eve's, so "Follow-up", "Hide sidebar", "Stories you might like" and /wiki/Star stay plain.
pub const ACCOUNT_VERBS: &[&str] = &[
    "upvote", "downvote", "vote", "unvote", "like", "unlike", "follow", "unfollow", "star", "unstar", "hide",
    "unhide", "flag", "unflag", "report", "save", "unsave", "subscribe", "unsubscribe", "bookmark",
];
/// After the verb, a word that names a piece of the page itself makes it a view toggle, not an account change:
/// "Hide sidebar", "Hide 12 replies", "Save as PDF", "Follow up".
pub const CHROME_NOUNS: &[&str] = &[
    "sidebar", "toolbar", "panel", "pane", "menu", "details", "preview", "navigation", "nav", "controls",
    "options", "filters", "keyboard", "banner", "header", "footer", "password", "replies", "comments", "thread",
    "images", "image", "map", "as", "up", "all", "more", "less",
];
/// Endpoints match the LAST path segment, lowercase as the action URLs are ("/vote", "/owner/repo/star"); a page
/// title like /wiki/Star is capitalised and /stargazers, /followers, /follow-us are other words. "report" is left
/// out: /report is as often a dashboard. The label rule still catches a "Report" link. (`_ACCOUNT_ENDPOINTS`:
/// ACCOUNT_VERBS minus "report".)
pub const ACCOUNT_ENDPOINTS: &[&str] = &[
    "upvote", "downvote", "vote", "unvote", "like", "unlike", "follow", "unfollow", "star", "unstar", "hide",
    "unhide", "flag", "unflag", "save", "unsave", "subscribe", "unsubscribe", "bookmark",
];
pub const ACTION_KEYS: &[&str] = &["action", "do", "op", "act", "cmd", "method"];
/// Python `_ENDPOINT_EXT`: `\.(?:php|json|html?|aspx?|cgi|jsp)$`.
pub static ENDPOINT_EXT: LazyLock<regex::Regex> = LazyLock::new(|| todo!("ax_candidates: _ENDPOINT_EXT"));
/// Python `_LEAD_SYMBOLS`: `^[^\w@#]+`.
pub static LEAD_SYMBOLS: LazyLock<regex::Regex> = LazyLock::new(|| todo!("ax_candidates: _LEAD_SYMBOLS"));
pub const TRAIL_PUNCT: &str = ".,:;!?…";
/// The shapes an account control's label takes after its verb. A count is digits with an optional k/m ("12",
/// "1.2k", "3,401"), never a price or a percentage. Python `_COUNT`: `^\d[\d,.]*[km]?$`, IGNORECASE.
pub static COUNT_RE: LazyLock<regex::Regex> = LazyLock::new(|| todo!("ax_candidates: _COUNT"));
/// Python `_HANDLE`: `^@\w[\w.\-]*$`.
pub static HANDLE_RE: LazyLock<regex::Regex> = LazyLock::new(|| todo!("ax_candidates: _HANDLE"));
pub const ACTED_ON: &[&str] = &[
    "this", "post", "comment", "story", "reply", "answer", "question", "video", "tweet", "repo", "repository",
    "photo", "track", "song", "playlist", "channel", "page", "link", "item",
];
/// Verbs whose object is a person or channel: "Follow Jane Doe" is a follow button's aria-label. As a link the
/// same shape is a title ("Follow the Money"), so this form counts on a button only.
pub const PERSON_VERBS: &[&str] = &["follow", "unfollow", "subscribe", "unsubscribe"];
/// Python `_SLUG_WORDS`: `[a-z0-9]+`.
pub static SLUG_WORDS: LazyLock<regex::Regex> = LazyLock::new(|| todo!("ax_candidates: _SLUG_WORDS"));
/// Mac apps whose web content (an AXWebArea) is a web page. Electron apps (Slack, Spotify, Discord, Notion) draw
/// their own chrome in an AXWebArea too; that is the app, so their "Follow" and "Save for later" stay unasked.
pub const BROWSER_APPS: &[&str] = &[
    "Safari", "Safari Technology Preview", "Google Chrome", "Google Chrome Canary", "Google Chrome Beta",
    "Chromium", "Arc", "Firefox", "Firefox Developer Edition", "Firefox Nightly", "Microsoft Edge", "Brave Browser",
    "Opera", "Vivaldi", "Orion", "DuckDuckGo", "Zen", "Zen Browser", "Dia", "Comet", "SigmaOS", "Waterfox", "LibreWolf",
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

/// Python's `urllib.parse.urlsplit(url)` (scheme lower-cased, no validation, never fails).
pub fn urlsplit(url: &str) -> UrlParts {
    todo!("ax_candidates: urlsplit")
}

/// Python's `urllib.parse.parse_qsl(query, keep_blank_values=keep_blank)`.
pub fn parse_qsl(query: &str, keep_blank: bool) -> Vec<(String, String)> {
    todo!("ax_candidates: parse_qsl")
}

/// Python's `urllib.parse.unquote(text)`.
pub fn unquote(text: &str) -> String {
    todo!("ax_candidates: unquote")
}

// ---- the sensitive and account rules -------------------------------------------------------------

/// True when a consequential verb is a whole word anywhere in the label. Fail-closed: a regex failure counts as
/// sensitive; only an empty (or all-space) label is not.
pub fn is_sensitive(label: &str) -> bool {
    todo!("ax_candidates: is_sensitive")
}

/// Python `_verb_shaped`: `words` (lead symbols gone) is an account verb and nothing, a count, what it acts on, or
/// a @handle; or, on a button, a follow-type verb and a capitalised name of at most two words.
pub fn verb_shaped(words: &[String], role: &str) -> bool {
    todo!("ax_candidates: _verb_shaped")
}

/// Python `_titles_its_destination`: the link goes to a page named after its own label (/wiki/Like for "Like",
/// ?q=hide for "Hide"). That is a title in a result list, not an action, however verb-like the label.
pub fn titles_its_destination(words: &[String], parts: &UrlParts) -> bool {
    todo!("ax_candidates: _titles_its_destination")
}

/// True when a page control changes the signed-in account: a link whose endpoint is an account verb, or a
/// verb-shaped label (leading ▲ ♥ ☆ ignored) that is not a link to a page named after it. A missing href is "".
pub fn is_account_change(label: &str, href: &str, role: &str) -> bool {
    todo!("ax_candidates: is_account_change")
}

/// `c` is on a web page: eve's own browser (app "browser"), or web content in a Mac browser (BROWSER_APPS).
pub fn is_web_page(c: &Candidate) -> bool {
    todo!("ax_candidates: is_web_page")
}

/// The lane asks the human before pressing `c`: a sensitive label anywhere, and on a web page (is_web_page) also
/// an account change by label or by `href`. The Mac lane's own "Save", "Hide" and "Flag", and an Electron app's
/// own chrome, stay unasked.
pub fn needs_confirmation(c: &Candidate, href: &str) -> bool {
    todo!("ax_candidates: needs_confirmation")
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

impl Candidate {
    /// `{"id", "role", "label", "value", "frame", "actions", "enabled", "app", "in_content", "in_dialog",
    /// "secure", "editable"}` in that order.
    pub fn to_dict(&self) -> Value {
        todo!("ax_candidates: Candidate.to_dict")
    }

    /// Python `Candidate.from_dict`: "id", "role" and "frame" are required (KeyError otherwise); the rest default.
    pub fn from_dict(d: &Value) -> anyhow::Result<Candidate> {
        todo!("ax_candidates: Candidate.from_dict")
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
        todo!("ax_candidates: Snapshot.href")
    }

    /// Python `Snapshot.get(cid)`.
    pub fn get(&self, cid: &str) -> Option<&Candidate> {
        todo!("ax_candidates: Snapshot.get")
    }

    /// `{"seq", "app", "bundle", "title", "pid", "elements", "context_lines", "focused", "dialog_text",
    /// "fullscreen", "node_count", "truncated", "secs"}` in that order.
    pub fn to_dict(&self) -> Value {
        todo!("ax_candidates: Snapshot.to_dict")
    }
}

/// Python `snapshot_from_dict`: the inverse of `Snapshot::to_dict` (Python's int()/float()/bool() coercions).
pub fn snapshot_from_dict(d: &Value) -> anyhow::Result<Snapshot> {
    todo!("ax_candidates: snapshot_from_dict")
}

pub fn is_pressable(c: &Candidate) -> bool {
    todo!("ax_candidates: is_pressable")
}

/// The labelled, enabled controls the lane could click (before the rank drops).
pub fn pressable(snapshot: &Snapshot) -> Vec<&Candidate> {
    todo!("ax_candidates: pressable")
}

// ---- deriving a snapshot from a raw walk ---------------------------------------------------------

/// Python `_text`: "" for null and bools, `str()` of a number, else whitespace-collapsed text.
pub fn text(v: &Value) -> String {
    todo!("ax_candidates: _text")
}

/// Python `_role_name`.
pub fn role_name(role: &str, subrole: &str) -> String {
    todo!("ax_candidates: _role_name")
}

/// Python `_descendant_text(node, depth=4)`: the first text under a node, in tree order (an image, button or link
/// title is only the fallback).
pub fn descendant_text(node: &Value, depth: usize) -> String {
    todo!("ax_candidates: _descendant_text")
}

/// Python `_value_text`.
pub fn value_text(role: &str, raw: &Value) -> String {
    todo!("ax_candidates: _value_text")
}

/// Python `_frame`: four finite numbers rounded to ints with a positive size, else None.
pub fn frame(raw: &Value) -> Option<Rect> {
    todo!("ax_candidates: _frame")
}

/// Python `_on_screen`.
pub fn on_screen(frame: Rect, screen: Option<(i64, i64)>) -> bool {
    todo!("ax_candidates: _on_screen")
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
    todo!("ax_candidates: snapshot_from_raw")
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
    todo!("ax_candidates: ax_snapshot")
}

// ---- the menu --------------------------------------------------------------------------------------

/// Casefolded words of ≥ 3 characters minus stop-words, in order, without duplicates.
pub fn objective_tokens(objective: &str) -> Vec<String> {
    todo!("ax_candidates: objective_tokens")
}

pub fn score_candidate(c: &Candidate, tokens: &[String]) -> i64 {
    todo!("ax_candidates: score_candidate")
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
    todo!("ax_candidates: rank_candidates")
}

/// "click radio button: Week (selected)" — URLs stripped, whitespace collapsed, label capped at `max_chars`
/// (Python default 40).
pub fn describe(c: &Candidate, verb: &str, max_chars: usize) -> String {
    todo!("ax_candidates: describe")
}

/// One line for the decider's state: app, title, focused control, a few static texts (Python default 300).
pub fn context_summary(snapshot: &Snapshot, max_chars: usize) -> String {
    todo!("ax_candidates: context_summary")
}

// ---- the real walker (macOS) -----------------------------------------------------------------------

/// Python `_app_identity`: (localizedName, bundleIdentifier) of a pid, ("", "") when unknown or off the Mac.
pub fn app_identity(pid: i64) -> (String, String) {
    todo!("ax_candidates: _app_identity")
}

/// The pid of a running app by its localized name (case-insensitive), 0 when not running (or off the Mac).
pub fn pid_for_app(name: &str) -> i64 {
    todo!("ax_candidates: pid_for_app")
}

/// The main display's size in points; None when there is no display (no viewport filter).
pub fn main_screen_points() -> Option<(i64, i64)> {
    todo!("ax_candidates: main_screen_points")
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
impl AxWalkBackend for MacAx {
    type Element = crate::ax_ffi::AxElement;

    fn app_identity(&self, pid: i64) -> (String, String) {
        todo!("ax_candidates: MacAx.app_identity")
    }

    fn window(&self, pid: i64) -> Option<Self::Element> {
        todo!("ax_candidates: MacAx.window")
    }

    fn fullscreen(&self, window: &Self::Element) -> bool {
        todo!("ax_candidates: MacAx.fullscreen")
    }

    fn values(&self, el: &Self::Element) -> Option<Vec<AxScalar<Self::Element>>> {
        todo!("ax_candidates: MacAx.values")
    }

    fn action_names(&self, el: &Self::Element) -> Option<Vec<String>> {
        todo!("ax_candidates: MacAx.action_names")
    }

    fn element_ref(&self, el: &Self::Element) -> AxRef {
        todo!("ax_candidates: MacAx.element_ref")
    }
}

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
    todo!("ax_candidates: ax_walk")
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
    todo!("ax_candidates: ax_walk (real backend)")
}

// ---- CLI: capture a raw walk for the fixtures ------------------------------------------------------

/// `python -m eve_bridge.ax_candidates [--app NAME] [--pid PID] [--max-nodes N] [--budget-secs S] [--objective
/// TEXT] [--json]`: walk one app's focused window and print its candidates. `argv` excludes the program name.
/// Returns the exit code.
pub fn main(argv: &[String]) -> i32 {
    todo!("ax_candidates: main")
}
