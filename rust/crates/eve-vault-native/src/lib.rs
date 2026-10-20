//! Putting a vault value into a page's field through the Accessibility API, for Codex tasks (port of
//! `vault_native.py`, used by `vault_mcp.py`).
//!
//! Codex drives the browser through its own cua_repl plugin and the ChatGPT extension, so eve cannot reach its
//! tab over CDP the way the browser lane does. It can reach the browser's Accessibility tree. Measured
//! 2026-10-12:
//!
//! * Codex's clicks (CDP events inside the tab) never give the browser the Mac's keyboard focus, and the
//!   browser need not be in front at all; key events would land in the address bar or another app. The first
//!   version typed at the Mac's focus and failed the real Codex run.
//! * The second looked for the field the PAGE had focused (AXFocused). Playwright's Chromium reports it in a
//!   background window because Playwright emulates page focus; the owner's Chrome, in the background during a
//!   real Codex task, reported no focused field at all. It failed the real run too.
//! * What holds: the tree has every window's active tab as an AXWebArea with its URL, and in it each text field
//!   with its kind (AXSecureTextField for a password) and what it is called (title, placeholder, DOM id,
//!   label). Setting AXValue on a field sets the page's value and fires its input and change events, the way
//!   autofill does; nothing comes to the front and no key is pressed.
//!
//! So a fill never types and never needs focus: [`Native::page_fields`] lists the text fields of the pages a
//! caller wants (`want(url)`: pages on other sites are never read), and vault_mcp's Server picks the field by
//! what it is. Focus, when a browser does report it, only breaks a tie. Nothing here reads a field's value: the
//! check afterwards is AXNumberOfCharacters. Only each window's ACTIVE tab is in the tree; a page in a
//! background tab is not seen.
//!
//! A browser builds its web Accessibility tree only while an assistive client is using it. The owner's Chrome
//! had it on (2026-10-12); a Codex computer task reads the same tree, which keeps it on. Chrome for Testing
//! keeps it off unless started with --force-renderer-accessibility (the live test does). Each browser is also
//! asked for AXManualAccessibility, the attribute Electron and some Chromium builds turn the tree on for; Chrome
//! for Testing and Chrome 2026-10 answer that it is unsupported (-25205), which costs nothing.
//!
//! The walk itself is written over [`AxTree`], so the same code walks the real Mac (`NativeMac`) and the fake
//! trees in the tests.

use std::any::Any;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

#[cfg(target_os = "macos")]
mod mac;

pub const TEXT_ROLES: [&str; 3] = ["AXTextField", "AXTextArea", "AXComboBox"];
pub const SECURE_SUBROLE: &str = "AXSecureTextField";
/// Per browser: a walk never runs away on a huge page.
pub const MAX_NODES: usize = 40000;
/// The browsers whose windows are walked. Kept sorted: Python walks `sorted(BROWSERS)`.
pub const BROWSERS: [&str; 12] = [
    "com.apple.Safari",
    "com.brave.Browser",
    "com.google.Chrome",
    "com.google.Chrome.beta",
    "com.google.Chrome.canary",
    "com.google.Chrome.dev",
    "com.google.chrome.for.testing",
    "com.microsoft.edgemac",
    "com.operasoftware.Opera",
    "com.vivaldi.Vivaldi",
    "company.thebrowser.Browser",
    "org.chromium.Chromium",
];
pub const NAME_ATTRS: [&str; 5] = ["AXTitle", "AXDescription", "AXPlaceholderValue", "AXDOMIdentifier", "AXHelp"];

/// A field's Accessibility element: opaque to callers, as Python's `element: Any` is. The real Mac stores its
/// AXUIElement here; a fake may store anything or nothing.
pub type Element = Arc<dyn Any + Send + Sync>;

/// One text field in a browser page. Never holds what is in the field.
#[derive(Clone, Default)]
pub struct Field {
    pub pid: i32,
    pub app: String,
    pub role: String,
    pub subrole: String,
    /// The URL of the page or frame the field is in.
    pub url: String,
    /// The URL of the top-level page it is in (= url outside a frame).
    pub page: String,
    /// What the field is called (label, placeholder, id), never what is in it.
    pub words: String,
    /// The page has it focused (some browsers say so only in the front window).
    pub focused: bool,
    /// Its place in its page, top to bottom.
    pub order: usize,
    pub element: Option<Element>,
}

impl Field {
    pub fn secure(&self) -> bool {
        self.subrole == SECURE_SUBROLE
    }
}

impl fmt::Debug for Field {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Field")
            .field("pid", &self.pid)
            .field("app", &self.app)
            .field("role", &self.role)
            .field("subrole", &self.subrole)
            .field("url", &self.url)
            .field("page", &self.page)
            .field("words", &self.words)
            .field("focused", &self.focused)
            .field("order", &self.order)
            .field("element", &self.element.as_ref().map(|_| "<element>"))
            .finish()
    }
}

fn elements_equal(a: &Option<Element>, b: &Option<Element>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => {
            if Arc::ptr_eq(x, y) {
                return true;
            }
            #[cfg(target_os = "macos")]
            if let Some(eq) = mac::same_element(x.as_ref(), y.as_ref()) {
                return eq;
            }
            false
        }
        _ => false,
    }
}

/// A frozen dataclass's `==`: every field, the element by CFEqual on the Mac (pyobjc's `==`), else by identity.
impl PartialEq for Field {
    fn eq(&self, other: &Self) -> bool {
        self.pid == other.pid
            && self.app == other.app
            && self.role == other.role
            && self.subrole == other.subrole
            && self.url == other.url
            && self.page == other.page
            && self.words == other.words
            && self.focused == other.focused
            && self.order == other.order
            && elements_equal(&self.element, &other.element)
    }
}

/// Hashes everything but the element, which keeps it consistent with `==`.
impl Hash for Field {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (self.pid, &self.app, &self.role, &self.subrole, &self.url, &self.page, &self.words, self.focused, self.order)
            .hash(state);
    }
}

/// What vault_mcp's Server needs from the Mac: `NativeMac`, or a test's fake with the same four methods.
pub trait Native {
    /// eve has the Accessibility permission. Python's Server treats a native without `trusted` as trusted, so a
    /// fake need not define it.
    fn trusted(&self) -> bool {
        true
    }
    /// Every text field in the pages whose URL `want` accepts, in every running browser's windows (each
    /// window's active tab). A page `want` refuses is not walked into: its fields are never read. A frame inside
    /// an accepted page must be accepted by `want` too, unless `frames` (a card: a payment provider's frame
    /// inside the checkout page).
    fn page_fields(&self, want: &dyn Fn(&str) -> bool, frames: bool) -> Vec<Field>;
    /// Sets the field's AXValue; true when the browser took it.
    fn set_value(&self, field: &Field, text: &str) -> bool;
    /// AXNumberOfCharacters of the field, when it says (never the value itself).
    fn char_count(&self, field: &Field) -> Option<i64>;
}

impl<T: Native + ?Sized> Native for &T {
    fn trusted(&self) -> bool {
        (**self).trusted()
    }
    fn page_fields(&self, want: &dyn Fn(&str) -> bool, frames: bool) -> Vec<Field> {
        (**self).page_fields(want, frames)
    }
    fn set_value(&self, field: &Field, text: &str) -> bool {
        (**self).set_value(field, text)
    }
    fn char_count(&self, field: &Field) -> Option<i64> {
        (**self).char_count(field)
    }
}

impl<T: Native + ?Sized> Native for Box<T> {
    fn trusted(&self) -> bool {
        (**self).trusted()
    }
    fn page_fields(&self, want: &dyn Fn(&str) -> bool, frames: bool) -> Vec<Field> {
        (**self).page_fields(want, frames)
    }
    fn set_value(&self, field: &Field, text: &str) -> bool {
        (**self).set_value(field, text)
    }
    fn char_count(&self, field: &Field) -> Option<i64> {
        (**self).char_count(field)
    }
}

/// An attribute's value as pyobjc hands it to Python: a CFString is a `str`, a CFBoolean a `bool`, a CFNumber
/// an `int` or a `float`, a CFURL an `NSURL` (here already its `absoluteString()`), an AXUIElement an element,
/// a CFArray a list, anything else an object whose `str()` is its description.
#[derive(Clone, Debug, PartialEq)]
pub enum AxValue<N> {
    Str(String),
    Bool(bool),
    Int(i64),
    Float(f64),
    Url(String),
    Element(N),
    Array(Vec<AxValue<N>>),
    Other(String),
}

impl<N> AxValue<N> {
    /// Python truthiness of the converted value.
    pub fn truthy(&self) -> bool {
        match self {
            AxValue::Str(s) => !s.is_empty(),
            AxValue::Bool(b) => *b,
            AxValue::Int(n) => *n != 0,
            AxValue::Float(x) => *x != 0.0,
            AxValue::Array(items) => !items.is_empty(),
            AxValue::Url(_) | AxValue::Element(_) | AxValue::Other(_) => true,
        }
    }

    /// Python's `str()` of the converted value (an element has no meaningful text; it never reaches here).
    pub fn py_str(&self) -> String {
        match self {
            AxValue::Str(s) | AxValue::Url(s) | AxValue::Other(s) => s.clone(),
            AxValue::Bool(b) => if *b { "True" } else { "False" }.to_string(),
            AxValue::Int(n) => n.to_string(),
            AxValue::Float(x) => py_float_str(*x),
            AxValue::Element(_) => "<AXUIElement>".to_string(),
            AxValue::Array(items) => {
                let inner: Vec<String> = items.iter().map(|v| v.py_str()).collect();
                format!("({})", inner.join(", "))
            }
        }
    }
}

fn py_float_str(x: f64) -> String {
    if x.is_nan() {
        "nan".to_string()
    } else if x.is_infinite() {
        if x > 0.0 { "inf" } else { "-inf" }.to_string()
    } else if x == x.trunc() && x.abs() < 1e16 {
        format!("{x:.1}")
    } else {
        format!("{x}")
    }
}

/// Python's `str.isspace` for one character: Unicode White_Space plus the four separators (U+001C to U+001F)
/// Python also counts.
fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// An Accessibility tree to walk: the real Mac, or a test's fake.
pub trait AxTree {
    type Node: Clone;
    /// `AXUIElementCopyAttributeValue`, `None` on any error (Python's `_attr`).
    fn attr(&self, node: &Self::Node, name: &str) -> Option<AxValue<Self::Node>>;
    /// The node as a [`Field`]'s element.
    fn element(&self, node: &Self::Node) -> Element;
}

/// A web area's URL: `absoluteString()` of its AXURL, or `str(raw or "")` when the browser gives no NSURL.
pub fn url_of<T: AxTree>(tree: &T, web_area: &T::Node) -> String {
    match tree.attr(web_area, "AXURL") {
        Some(AxValue::Url(s)) => s,
        Some(v) if v.truthy() => v.py_str(),
        _ => String::new(),
    }
}

/// What a field is called: its title, description, placeholder, DOM id and help, then its label element's value
/// (or title). Only non-blank strings count; at most 200 characters.
pub fn words_of<T: AxTree>(tree: &T, element: &T::Node) -> String {
    let mut parts: Vec<Option<AxValue<T::Node>>> = NAME_ATTRS.iter().map(|a| tree.attr(element, a)).collect();
    if let Some(label) = tree.attr(element, "AXTitleUIElement") {
        let named = match &label {
            AxValue::Element(node) => match tree.attr(node, "AXValue") {
                Some(v) if v.truthy() => Some(v),
                _ => tree.attr(node, "AXTitle"),
            },
            // not an element: nothing to ask, and nothing a string check would keep
            _ => None,
        };
        parts.push(named);
    }
    let kept: Vec<String> = parts
        .into_iter()
        .filter_map(|p| match p {
            Some(AxValue::Str(s)) if !s.trim_matches(py_space).is_empty() => Some(s),
            _ => None,
        })
        .collect();
    kept.join(" ").chars().take(200).collect()
}

/// AXNumberOfCharacters as Python reads it: `int(n) if isinstance(n, int) else None` (a bool is an int).
pub fn count_of<N>(value: Option<AxValue<N>>) -> Option<i64> {
    match value {
        Some(AxValue::Int(n)) => Some(n),
        Some(AxValue::Bool(b)) => Some(i64::from(b)),
        _ => None,
    }
}

fn children<N: Clone>(value: Option<AxValue<N>>) -> Vec<N> {
    match value {
        Some(AxValue::Array(items)) => items
            .into_iter()
            .filter_map(|v| match v {
                AxValue::Element(n) => Some(n),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// One browser's part of `page_fields`: walks the windows of `app` (its application element) depth first, top
/// to bottom, at most [`MAX_NODES`] nodes. A web area `want` refuses is skipped whole; a text field inside an
/// accepted page is recorded and not walked into.
pub fn walk_app<T: AxTree>(
    tree: &T,
    pid: i32,
    name: &str,
    app: &T::Node,
    want: &dyn Fn(&str) -> bool,
    frames: bool,
) -> Vec<Field> {
    let mut out = Vec::new();
    let mut seen = 0usize;
    let mut stack: Vec<(T::Node, String, String)> =
        children(tree.attr(app, "AXWindows")).into_iter().map(|w| (w, String::new(), String::new())).collect();
    while seen < MAX_NODES {
        let Some((node, mut url, mut page)) = stack.pop() else { break };
        seen += 1;
        let role = tree.attr(&node, "AXRole");
        let role_str = match &role {
            Some(AxValue::Str(s)) => Some(s.as_str()),
            _ => None,
        };
        if role_str == Some("AXWebArea") {
            url = url_of(tree, &node);
            if !(want(&url) || (frames && !page.is_empty())) {
                continue;
            }
            if page.is_empty() {
                page = url.clone();
            }
        } else if !url.is_empty() && role_str.is_some_and(|r| TEXT_ROLES.contains(&r)) {
            let subrole = match tree.attr(&node, "AXSubrole") {
                Some(v) if v.truthy() => v.py_str(),
                _ => String::new(),
            };
            out.push(Field {
                pid,
                app: name.to_string(),
                role: role_str.unwrap_or_default().to_string(),
                subrole,
                url,
                page,
                words: words_of(tree, &node),
                focused: matches!(tree.attr(&node, "AXFocused"), Some(AxValue::Bool(true))),
                order: seen,
                element: Some(tree.element(&node)),
            });
            continue;
        }
        let kids = children(tree.attr(&node, "AXChildren"));
        stack.extend(kids.into_iter().rev().map(|child| (child, url.clone(), page.clone())));
    }
    out
}

/// The real Mac. Tests use a fake with the same four methods (see [`Native`]).
pub struct NativeMac {
    #[cfg(target_os = "macos")]
    _mac: (),
    #[cfg(not(target_os = "macos"))]
    never: std::convert::Infallible,
}

impl NativeMac {
    /// Fails off macOS, as the Python constructor raises `RuntimeError`.
    pub fn new() -> anyhow::Result<Self> {
        #[cfg(target_os = "macos")]
        {
            Ok(NativeMac { _mac: () })
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err(anyhow::anyhow!("the vault fills fields only on macOS"))
        }
    }
}

#[cfg(not(target_os = "macos"))]
impl Native for NativeMac {
    fn trusted(&self) -> bool {
        match self.never {}
    }
    fn page_fields(&self, _want: &dyn Fn(&str) -> bool, _frames: bool) -> Vec<Field> {
        match self.never {}
    }
    fn set_value(&self, _field: &Field, _text: &str) -> bool {
        match self.never {}
    }
    fn char_count(&self, _field: &Field) -> Option<i64> {
        match self.never {}
    }
}

#[cfg(test)]
mod tests;
