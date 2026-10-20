//! The browser lane: a browser drives a web task, the plan executor and Jev decide each step.
//!
//! The fast lane's ceiling on the Mac is the Accessibility tree over web content: nameless buttons, stale
//! frames, coordinate clicks (Jev's holdout on the AX fixtures: 70.8 %, docs/stackchan/routing.md). A browser
//! driven over the DevTools protocol has none of that. Its page gives every control a role and an accessible
//! name, a click lands on the element and not on a point, and "did it work" is a DOM question with an exact
//! answer (the URL, the title, the text that is now on the page). Measured on this Mac, 2026-09-21: Chromium up
//! in 0.19 s, a page in 0.52 s, a snapshot of every control in 40 ms.
//!
//! This module changes the eyes and hands, not the brain. It presents a page as the same `Snapshot` of
//! `Candidate`s that ax_candidates builds from a window, so `plan_executor::run_plan` — astra's one plan, the
//! keyword gate, Jev's typed step answer, the sensitive-label table, "only the human approves" — runs unchanged
//! on top of it. Nothing about approval is reimplemented here.
//!
//! ```text
//!     goal ── is it a web task? (WEB_WORDS, a URL, the router's search/open_url) ──▶ BrowserLane::run_plan
//!                                                                                    │ outline of the page
//!                                                                                    ▼
//!                                                   astra plans once (plan_contract.plan_request)
//!                                                                                    │ Plan
//!                                                                                    ▼
//!                               plan_executor::run_plan(senses=Page, effectors=Page, asker=Jev)
//!                                                                                    │ per step: candidates from
//!                                                                                    │ the DOM, gate + Jev pick,
//!                                                                                    ▼ the page clicks, expect
//!                                                             complete → one sentence · partial → today's loop
//! ```
//!
//! Production drives the owner's own Chrome (ATTACH MODE, below). The lane can also run eve's own persistent
//! Chromium profile under `~/.config/eve-bridge/browser`; only tools and benches use that.
//!
//! ATTACH MODE (owner, 2026-09-23: "i want eve to be able to control logged in browser, that's the most
//! important"). With `EVE_BROWSER_ATTACH=1` the lane drives the owner's own running Chrome, signed in to
//! everything, instead of its own profile. Chrome 136+ ignores `--remote-debugging-port` on the default profile;
//! the supported route (Chrome 144+) is the owner switching on remote debugging once at
//! `chrome://inspect/#remote-debugging`, after which Chrome writes `DevToolsActivePort` (port, then the browser's
//! WebSocket path) into its user data directory. [`devtools_endpoint`] reads that file exactly as Google's
//! chrome-devtools-mcp `--autoConnect` does, and the driver attaches over CDP.
//! EVE'S OWN CHROME (owner, 2026-09-27: "lets give eve its own profile"). With `EVE_BROWSER_OWN=1` the lane
//! drives a second Chrome that belongs to eve: real Google Chrome, started with its own user data directory
//! (`~/.config/eve-bridge/chrome`, `EVE_OWN_CHROME_DIR` moves it) and `--remote-debugging-port=0`. Chrome refuses
//! that flag only on the owner's default directory, so this Chrome never asks "Allow remote debugging?" and
//! nobody presses anything. It stays open between tasks with the owner's sign-ins for eve (`eve-bridge
//! chrome-profile` opens it to sign in). It is plain Chrome, not a driver's launch, so it carries no automation
//! flags and Google's sign-in accepts it. The trade: any program on the Mac can drive this Chrome through its
//! port, so it holds only the accounts the owner signs into it, never the owner's own Chrome. Own mode is attach
//! mode with a different endpoint: the same tab rules and the same gate apply.
//! STEEL (owner, 2026-10-13: "replace browser with steel for eve", "local steel"). With `EVE_BROWSER_STEEL=1`
//! the lane drives a Chrome run by the self-hosted Steel browser API on this Mac (eve-steel-local): eve starts the
//! server when nothing answers, opens a Steel session and attaches to its websocket. No API key, no window, no
//! Allow; it turns the lane on by itself and takes precedence over own and attach mode. Its profile keeps eve's
//! sign-ins in `~/.config/eve-bridge/steel-profile`. Closing the lane releases the session.
//!
//! In attach mode the lane works in a NEW TAB of its own — never one of the owner's — and closing it closes that
//! tab and disconnects: the owner's browser, windows and tabs are never closed. The same gate applies as
//! everywhere: a sensitive control (buy, send, delete, pay…) stops the plan for the owner's yes.
//!
//! The lane owns one worker thread and everything that touches the browser runs there; the daemon awaits it
//! ([`BrowserLane`] over [`LaneCore`]).

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc as std_mpsc, Arc, LazyLock};
use std::time::{Duration, Instant};

use eve_desktop_lanes::ax_candidates::{is_sensitive, needs_confirmation, Candidate, Snapshot};
use eve_desktop_lanes::fast_lane::{Effectors, Senses, StepAsk};
use eve_desktop_lanes::plan_executor::{run_plan, PlanOptions};
use eve_desktop_lanes::{casefold, error_kind, error_message, py_collapse, py_is_space, py_round_n, py_strip, LaneError};
use eve_vault::pystr::{repr, str_or_empty, truthy, value_repr, value_str};
use eve_vault::pyurl::urlsplit;
use indexmap::IndexMap;
use regex::Regex;
use serde::Serialize;
use serde_json::{json, Map, Value};
use sha1::{Digest, Sha1};

use crate::driver::{same_context, BrowserHandle, ContextHandle, Driver, PageHandle, SelectBy, StartDriver};
pub use crate::js::{COLLECT_JS, CONSENT_JS, FILL_FIELDS_JS, IDENTITY_JS, RECHECK_JS, WEBMCP_JS};

pub const DEFAULT_PROFILE: &str = "~/.config/eve-bridge/browser";
/// the owner's Chrome user data directory
pub const DEFAULT_CHROME_DIR: &str = "~/Library/Application Support/Google/Chrome";
/// eve's own Chrome (see EVE'S OWN CHROME)
pub const DEFAULT_OWN_CHROME_DIR: &str = "~/.config/eve-bridge/chrome";
pub const CHROME_APP: &str = "Google Chrome";
/// eve's Chrome: how long a cold start may take to open its debugging port
pub const OWN_START_SECS: f64 = 20.0;
/// page_text: how long a still-growing page may take to settle
pub const READ_WAIT_SECS: f64 = 6.0;
pub const READ_POLL_SECS: f64 = 0.5;
/// below this a page is still loading (Gmail's shell is ~190)
pub const READ_ENOUGH_CHARS: usize = 400;
/// what chrome://inspect/#remote-debugging shows as "Server running at"
pub const DEFAULT_DEBUG_PORT: i64 = 9222;
/// Chrome asks the owner "Allow remote debugging?" per connection: wait for the click
pub const CONSENT_TIMEOUT_MS: u64 = 120_000;
pub const DEFAULT_VIEWPORT: (u32, u32) = (1280, 860);
pub const MAX_CANDIDATES: usize = 120;
pub const SNAPSHOT_TIMEOUT_MS: u64 = 3000;
pub const NAV_READ_POLL_MS: u64 = 50;
pub const NAV_TIMEOUT_MS: u64 = 15000;
pub const SETTLE_CAP_SECS: f64 = 3.0;
/// Return in a form: how long to wait for the navigation to start
pub const ENTER_NAV_SECS: f64 = 0.6;
/// The keys a plan may press, and the browser's name for each.
pub const KEYS: [(&str, &str); 10] = [
    ("return", "Enter"),
    ("enter", "Enter"),
    ("escape", "Escape"),
    ("tab", "Tab"),
    ("space", " "),
    ("up", "ArrowUp"),
    ("down", "ArrowDown"),
    ("left", "ArrowLeft"),
    ("right", "ArrowRight"),
    ("delete", "Backspace"),
];

/// `KEYS.get(name)`.
pub fn key_name(name: &str) -> Option<&'static str> {
    KEYS.iter().find(|(k, _)| *k == name).map(|(_, v)| *v)
}

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("a fixed pattern")
}

pub static SIGN_IN: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)(/login|/signin|/sign_in|/ServiceLogin|accounts\.google\.com/.*(signin|ServiceLogin)|/ap/signin)")
});

/// A goal for the web: a URL, a site, or the browser named. Anything else stays on the Mac lane.
pub static WEB_WORDS: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!(
        r"(?i)(https?://|www\.|\.(com|org|net|io|ai|gov|edu)\b|\b(browser|chrome|safari|website|web ?page|webpage|",
        r"google|youtube|amazon|github|wikipedia|reddit|twitter|linkedin|gmail|news|hacker ?news|bbc|nytimes|",
        r"stack ?overflow|search (the )?web|look up|google it)\b)"
    ))
});

/// click_candidate: the page moved under the decision; nothing was clicked
pub const STALE_PREFIX: &str = "refused: stale:";
/// every effector on a bot-check page (browser_blocker)
pub const BLOCKED_PREFIX: &str = "refused: blocked:";

// What a live page changes in a label on its own: counts ("142 points", "1.2K views") and relative times
// ("3 minutes ago", "4h", "just now"). Both are folded to one "#" before the names are compared, and only for a
// control whose role and href (or role and DOM id) are unchanged, so it is provably the same control.
static VOLATILE_NUMBER: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    fancy_regex::Regex::new(r"(?i)\d[\d,.]*(?:\s*[kmb](?![a-z]))?").expect("a fixed pattern")
});
static VOLATILE_TIME: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!(
        r"(?i)\b(?:just now|now|ago|today|yesterday|seconds?|secs?|minutes?|mins?|hours?|hrs?|",
        r"days?|weeks?|wks?|months?|mos?|years?|yrs?|[smhdwy])\b"
    ))
});
static VOLATILE_RUN: LazyLock<Regex> = LazyLock::new(|| re(r"(?:#\s*)+"));
static MONEY: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)[$€£¥₹]|\b(?:usd|eur|gbp|price|total|pay|buy|order|donate|tip|checkout)\b"));

/// `name` with every count and relative-time word folded to "#": "3 minutes ago" and "1 day ago" match.
pub fn volatile_free(name: &str) -> String {
    let collapsed = casefold(&py_collapse(name));
    let numbers = VOLATILE_NUMBER.replace_all(&collapsed, "#").into_owned();
    let folded = VOLATILE_TIME.replace_all(&numbers, "#");
    py_strip(&VOLATILE_RUN.replace_all(&folded, "# ")).to_string()
}

/// The URL with its #fragment dropped: an in-page jump or a hash router is the same document's page.
pub fn without_fragment(url: Option<&Value>) -> String {
    let text = str_or_empty(url);
    text.split('#').next().unwrap_or("").to_string()
}

/// What differs between the control as the decision saw it and as the page has it now, in words; [] when it is
/// the same control in the same document. Anything unreadable is "control": fail closed.
/// The #fragment of the page address is ignored. A name may differ in counts and relative times only when the
/// role and a non-empty href or DOM id are unchanged ([`volatile_free`]); any other change to it counts.
/// After LayaBrowse's decision_still_valid (MIT, Copyright (c) 2026 Aryan Bhujade).
pub fn click_recheck(recorded: &Value, now: &Value) -> Vec<String> {
    let Some(now_map) = now.as_object() else {
        return vec!["control".to_string()];
    };
    if !now_map.get("found").is_some_and(truthy) {
        return vec!["control".to_string()];
    }
    let rec = |key: &str| str_or_empty(recorded.get(key));
    let cur = |key: &str| str_or_empty(now_map.get(key));
    let same = |key: &str| rec(key) == cur(key);
    let mut changed: Vec<String> = Vec::new();
    if without_fragment(recorded.get("url")) != without_fragment(now_map.get("url")) {
        changed.push("page address".into());
    }
    for (key, word) in [("doc", "document"), ("role", "role")] {
        if !same(key) {
            changed.push(word.into());
        }
    }
    if !same("name") {
        let (before, after) = (rec("name"), cur("name"));
        let anchored = same("role") && ((same("href") && !cur("href").is_empty()) || (same("dom_id") && !cur("dom_id").is_empty()));
        // On a control the owner is asked about, or one that names money, the number is what was approved.
        let foldable = ![&before, &after].iter().any(|n| is_sensitive(n) || MONEY.is_match(n));
        if !(anchored && foldable && volatile_free(&before) == volatile_free(&after)) {
            changed.push("name".into());
        }
    }
    if !same("href") {
        changed.push("href".into());
    }
    changed
}

// Bot checks: pages that stand between eve and the site until a human proves they are one. They are not the
// task's page and this lane must never try to solve one (no "I'm not a robot" clicks), so the lane stops with a
// line for the owner instead of handing the page on to a planner that would.
// Ported from LayaBrowse goals.browser_blocker (MIT, Copyright (c) 2026 Aryan Bhujade), with Cloudflare's
// interstitial added. Google's /sorry/ counts only on a Google host: "/sorry/" alone is an ordinary path.
pub const CHALLENGE_TITLES: [&str; 5] =
    ["pardon our interruption", "access denied", "robot check", "just a moment", "attention required! | cloudflare"];
pub const CHALLENGE_TEXT: [&str; 3] = [
    "checking your browser before you access",
    "checking your browser before accessing",
    "needs to review the security of your connection before proceeding",
];
/// the phrases count only at the top of the page, where the check says them
pub const CHALLENGE_HEAD_CHARS: usize = 500;
// Cloudflare's JavaScript check ("Just a moment…", "Checking your browser…", "…review the security of your
// connection…") usually passes on its own in about five seconds and then loads the real page, so the lane looks
// again for up to CHALLENGE_SETTLE_SECS before it blocks. Every other check is hard: it never clears by waiting,
// and the lane stops at once.
pub const SOFT_CHALLENGE_TITLES: [&str; 1] = ["just a moment"];
pub const CHALLENGE_SETTLE_SECS: f64 = 8.0;
pub const CHALLENGE_POLL_SECS: f64 = 1.0;

/// "hard" for a bot check that waiting never clears, "soft" for Cloudflare's self-clearing JavaScript check, ""
/// for an ordinary page. Pure: the URL, the title, the text.
pub fn challenge_kind(url: &str, title: &str, text: &str) -> &'static str {
    let (host, path) = match urlsplit(url) {
        Ok(parts) => (parts.hostname().unwrap_or_default(), parts.path),
        Err(_) => (String::new(), String::new()),
    };
    if path.starts_with("/splashui/challenge") {
        return "hard";
    }
    if path.starts_with("/sorry/") && (host == "google.com" || host.ends_with(".google.com")) {
        return "hard";
    }
    let folded = casefold(title);
    let name = py_strip(py_strip(&folded).trim_end_matches(['.', '…']));
    if CHALLENGE_TITLES.contains(&name) {
        return if SOFT_CHALLENGE_TITLES.contains(&name) { "soft" } else { "hard" };
    }
    let top: String = text.chars().take(CHALLENGE_HEAD_CHARS).collect();
    let head = casefold(&py_collapse(&top));
    if CHALLENGE_TEXT.iter().any(|phrase| head.contains(phrase)) {
        return "soft";
    }
    ""
}

/// "browser_challenge" when the page is a known bot check ([`challenge_kind`], soft or hard), else "".
pub fn browser_blocker(url: &str, title: &str, text: &str) -> &'static str {
    if challenge_kind(url, title, text).is_empty() {
        ""
    } else {
        "browser_challenge"
    }
}

/// What eve tells the owner when a bot check stops the lane: the site (never the path or query) and what to do.
pub fn blocked_line(url: &str) -> String {
    let site = observed_site(url);
    let host = site.strip_prefix("www.").unwrap_or(&site);
    let host = if host.is_empty() { "that site" } else { host };
    format!(
        "{host} is showing a bot check, so I stopped: I can't get past it myself. Open {host} in Chrome, finish the \
         check, then ask me again."
    )
}

// A cookie or consent notice covering the page. Sites put it in a dialog, which the lane (rightly) never
// operates, so every click stopped at dialog_open and no plan dismissed it first (browser_model_eval
// cookie_banner, 2026-09-24: 0 of 15 runs across five models). Code dismisses it before a click or a type: only
// a notice whose text is about cookies or consent, and only with a button that REFUSES or closes — never one
// that agrees. A label the sensitive table knows ("Accept all", "Agree", "OK") is never pressed here; a notice
// that offers nothing else stays up, and the step stops at dialog_open as before.
/// CONSENT_TEXT's pattern, as the page's own RegExp is built from it (`_CONSENT_JS`).
pub const CONSENT_TEXT_PATTERN: &str =
    r"\b(cookies?|consent|gdpr|tracking technologies|privacy (settings|preferences|choices))\b";
pub static CONSENT_TEXT: LazyLock<Regex> = LazyLock::new(|| re(&format!("(?i){CONSENT_TEXT_PATTERN}")));
pub static CONSENT_REFUSE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)\b(reject|refuse|deny|necessary|essential|required only|only required|no,? thanks)\b")
});
pub static CONSENT_CLOSE: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^\s*(close|dismiss|not now|×|✕|x)\s*$"));

/// The button that dismisses a consent notice without agreeing to anything, or None: a refusal first ("Reject
/// non-essential cookies", "Necessary only"), else a plain close. A sensitive label never.
pub fn consent_choice(buttons: &[Value]) -> Option<Value> {
    for rx in [&*CONSENT_REFUSE, &*CONSENT_CLOSE] {
        for b in buttons {
            let name = str_or_empty(b.get("name"));
            if !name.is_empty() && rx.is_match(&name) && !is_sensitive(&name) {
                return Some(b.clone());
            }
        }
    }
    None
}

/// The collector's role, in the words the gate and Jev read.
pub const ROLE_WORDS: [(&str, &str); 14] = [
    ("link", "link"),
    ("button", "button"),
    ("checkbox", "checkbox"),
    ("radio", "radio button"),
    ("searchbox", "search field"),
    ("textbox", "text field"),
    ("password", "secure text field"),
    ("combobox", "combo box"),
    ("tab", "tab"),
    ("menuitem", "menu item"),
    ("option", "option"),
    ("switch", "switch"),
    ("menuitemcheckbox", "checkbox"),
    ("menuitemradio", "radio button"),
];

fn role_word(role: &str) -> Option<&'static str> {
    ROLE_WORDS.iter().find(|(k, _)| *k == role).map(|(_, v)| *v)
}

// ---- configuration --------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserLaneConfig {
    pub profile: PathBuf,
    pub headless: bool,
    /// drive the owner's running Chrome (see ATTACH MODE)
    pub attach: bool,
    pub chrome_dir: PathBuf,
    pub debug_port: i64,
    /// attach: the Google account whose Chrome profile to work in
    pub chrome_profile: String,
    /// drive eve's own Chrome, not the owner's (implies attach)
    pub own: bool,
    pub own_dir: PathBuf,
    /// drive a local Steel session (see STEEL; implies attach)
    pub steel: bool,
}

impl Default for BrowserLaneConfig {
    fn default() -> Self {
        BrowserLaneConfig {
            profile: eve_steel_local::py_path(DEFAULT_PROFILE),
            headless: false,
            attach: false,
            chrome_dir: eve_steel_local::py_path(DEFAULT_CHROME_DIR),
            debug_port: DEFAULT_DEBUG_PORT,
            chrome_profile: String::new(),
            own: false,
            own_dir: eve_steel_local::py_path(DEFAULT_OWN_CHROME_DIR),
            steel: false,
        }
    }
}

/// `(env.get(name) or "")` from `environ`, else the process environment.
fn env_get(environ: Option<&HashMap<String, String>>, name: &str) -> String {
    match environ {
        Some(env) => env.get(name).cloned().unwrap_or_default(),
        None => std::env::var(name).unwrap_or_default(),
    }
}

/// `(env.get(name) or "0").strip().lower() in ("1", "true", "yes", "on")`.
fn env_on(environ: Option<&HashMap<String, String>>, name: &str) -> bool {
    let raw = env_get(environ, name);
    let raw = if raw.is_empty() { "0".to_string() } else { raw };
    matches!(py_strip(&raw).to_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

/// `EVE_BROWSER_ATTACH=1` drives the owner's running Chrome (`EVE_CHROME_DIR` moves where its
/// `DevToolsActivePort` is looked for; `EVE_CHROME_DEBUG_PORT` is the fallback port; `EVE_CHROME_PROFILE` names
/// the Google account whose Chrome profile to work in). `EVE_BROWSER_OWN=1` drives eve's own Chrome instead, and
/// turns the lane on by itself (`EVE_OWN_CHROME_DIR` moves its user data directory). `EVE_BROWSER_STEEL=1` drives
/// a local Steel session instead of either (eve-steel-local), and also turns the lane on by itself.
/// `environ` None reads the process environment.
pub fn configured(environ: Option<&HashMap<String, String>>) -> BrowserLaneConfig {
    let steel = env_on(environ, "EVE_BROWSER_STEEL");
    let own = !steel && env_on(environ, "EVE_BROWSER_OWN");
    let attach = steel || own || env_on(environ, "EVE_BROWSER_ATTACH");
    let dir = |name: &str, default: &str| {
        let raw = env_get(environ, name);
        let raw = py_strip(&raw);
        eve_steel_local::py_path(if raw.is_empty() { default } else { raw })
    };
    let port_raw = env_get(environ, "EVE_CHROME_DEBUG_PORT");
    let debug_port = if port_raw.is_empty() {
        DEFAULT_DEBUG_PORT
    } else {
        eve_vault::pystr::int_of_str(&port_raw)
            .map(|n| n.clamp(i64::MIN as i128, i64::MAX as i128) as i64)
            .unwrap_or(DEFAULT_DEBUG_PORT)
    };
    BrowserLaneConfig {
        profile: eve_steel_local::py_path(DEFAULT_PROFILE),
        headless: false,
        attach,
        chrome_dir: dir("EVE_CHROME_DIR", DEFAULT_CHROME_DIR),
        debug_port,
        chrome_profile: py_strip(&env_get(environ, "EVE_CHROME_PROFILE")).to_lowercase(),
        own,
        own_dir: dir("EVE_OWN_CHROME_DIR", DEFAULT_OWN_CHROME_DIR),
        steel,
    }
}

// ---- which Chrome profile ---------------------------------------------------------------------------------------

// Which Chrome profile a context is: the Google accounts signed in there, from Google's own account list, fetched
// with THAT profile's cookies — no tab opens. The first listed is the profile's primary account.
pub const ACCOUNTS_URL: &str = "https://accounts.google.com/ListAccounts?gpsia=1&source=ChromiumBrowser&json=standard";
pub static EMAIL: LazyLock<Regex> = LazyLock::new(|| re(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}"));
/// The whole lookup, every profile at once, gets this long. Production, 2026-09-23/24: one request after another
/// at 8 s each cost 7 s and 19 s before a task began, and identified nothing; the one quick run took 1.4 s.
pub const PROFILE_LOOKUP_BUDGET_SECS: f64 = 3.0;
pub const CHROME_UA: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";

/// Google's account list, asked with one profile's cookies. Runs on a lookup thread, never the lane's.
pub fn fetch_accounts(url: &str, cookie_header: &str, timeout: f64) -> anyhow::Result<String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::try_from_secs_f64(timeout).unwrap_or(Duration::from_secs(3)))
        .build()?;
    let reply = client
        .get(url)
        .header("Cookie", cookie_header)
        .header("User-Agent", CHROME_UA)
        .send()?
        .error_for_status()?;
    Ok(String::from_utf8_lossy(&reply.bytes()?).into_owned())
}

/// The signed-in accounts in a ListAccounts reply, primary first, deduplicated, lower-cased.
pub fn accounts_in(body: &str) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for m in EMAIL.find_iter(body) {
        let e = m.as_str().to_lowercase();
        if !seen.contains(&e) {
            seen.push(e);
        }
    }
    seen
}

static WORDS: LazyLock<Regex> = LazyLock::new(|| re(r"[a-z0-9.@-]+"));

/// The profile (primary account) a request names — an email, its part before @, or its domain word ("era",
/// "uw", "gmail") — else `default` if open, else "" (the first profile).
pub fn profile_for<V>(request: &str, profiles: &IndexMap<String, V>, default: &str) -> String {
    let lowered = request.to_lowercase();
    let words: HashSet<&str> = WORDS.find_iter(&lowered).map(|m| m.as_str()).collect();
    for email in profiles.keys() {
        let (local, domain) = email.split_once('@').unwrap_or((email.as_str(), ""));
        let first = domain.split('.').next().unwrap_or("");
        if [email.as_str(), local, domain, first].iter().any(|n| words.contains(n)) {
            return email.clone();
        }
    }
    if profiles.contains_key(default) {
        default.to_string()
    } else {
        String::new()
    }
}

// ---- the owner's Chrome, eve's own Chrome -----------------------------------------------------------------------

/// Whether something accepts a connection on 127.0.0.1:`port` within half a second.
pub fn listening(port: i64) -> bool {
    let Ok(port) = u16::try_from(port) else { return false };
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok()
}

/// The running Chrome's browser WebSocket, or None when remote debugging is off or Chrome is not running.
///
/// First `<chrome_dir>/DevToolsActivePort` (a port line, then a path line), the same reading as
/// chrome-devtools-mcp's autoConnect. macOS's app-data protection refuses that read to other apps ("Operation
/// not permitted", 2026-09-23), so when the file cannot be read the endpoint is Chrome's debugging `port` (the
/// one chrome://inspect shows) at `/devtools/browser` — Chrome then asks the owner "Allow remote debugging?" for
/// the connection, and that click, not the file, is the gate.
pub fn devtools_endpoint(chrome_dir: &Path, port: i64) -> Option<String> {
    devtools_endpoint_with(chrome_dir, port, &listening)
}

/// [`devtools_endpoint`] with the port probe as a parameter (the tests' `_listening`).
pub fn devtools_endpoint_with(chrome_dir: &Path, port: i64, listening: &dyn Fn(i64) -> bool) -> Option<String> {
    let text = match std::fs::read(chrome_dir.join("DevToolsActivePort")) {
        Ok(bytes) => String::from_utf8(bytes).ok()?,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            return listening(port).then(|| format!("ws://127.0.0.1:{port}/devtools/browser"));
        }
        Err(_) => return None,
    };
    let lines: Vec<&str> = text.lines().map(py_strip).filter(|l| !l.is_empty()).collect();
    if lines.len() < 2 || !is_digits(lines[0]) || !lines[1].starts_with("/devtools/browser/") {
        return None;
    }
    let port = eve_vault::pystr::int_of_str(lines[0])?;
    (0 < port && port < 65536).then(|| format!("ws://127.0.0.1:{port}{}", lines[1]))
}

/// `str.isdigit()` over a whole non-empty string, for the Unicode decimal digits `int()` reads.
fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| eve_vault::pystr::int_of_str(&c.to_string()).is_some() && !py_is_space(c))
}

/// Run `argv` and return what it wrote to stdout (`subprocess.run(argv, capture_output=True, text=True,
/// timeout=)`): a missing program or a timeout is an error, a failing exit status is not.
pub fn run_command(argv: &[String], timeout_secs: f64) -> anyhow::Result<String> {
    let Some((program, args)) = argv.split_first() else {
        anyhow::bail!("no command");
    };
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let mut stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        if let Some(s) = stdout.as_mut() {
            let _ = std::io::Read::read_to_end(s, &mut out);
        }
        out
    });
    let mut stderr = child.stderr.take();
    std::thread::spawn(move || {
        let mut sink = Vec::new();
        if let Some(s) = stderr.as_mut() {
            let _ = std::io::Read::read_to_end(s, &mut sink);
        }
    });
    let deadline = deadline_after(Instant::now(), timeout_secs);
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("Command {:?} timed out after {} seconds", argv, timeout_secs);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let out = reader.join().unwrap_or_default();
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// `start + secs`, saturating far in the future for a huge or infinite `secs` (never a panic).
fn deadline_after(start: Instant, secs: f64) -> Instant {
    let far = Duration::from_secs(100 * 365 * 24 * 3600);
    let wait = Duration::try_from_secs_f64(secs.max(0.0)).unwrap_or(far).min(far);
    start.checked_add(wait).unwrap_or(start + Duration::from_secs(3600))
}

/// `run(argv, timeout)` → stdout: the subprocess seam the Python passed as `run=`.
pub type Run<'a> = &'a dyn Fn(&[String], f64) -> anyhow::Result<String>;

fn argv(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// Chrome refuses a remote-debugging connection with 403 when it has NO window — the "Allow?" prompt has nowhere
/// to show (live, 2026-09-23: Chrome running, zero windows, 403 at once; one window, connected). On a Mac,
/// closing every window leaves Chrome running, so this is common: open a plain new window if there is none. True
/// when Chrome has a window afterwards.
pub fn ensure_chrome_window() -> anyhow::Result<bool> {
    ensure_chrome_window_with(&run_command, &|s| std::thread::sleep(Duration::from_secs_f64(s)))
}

/// [`ensure_chrome_window`] with the subprocess and the sleep as parameters.
pub fn ensure_chrome_window_with(run: Run<'_>, sleep: &dyn Fn(f64)) -> anyhow::Result<bool> {
    let windows = || -> anyhow::Result<i64> {
        let out = run(
            &argv(&[
                "osascript",
                "-e",
                "if application \"Google Chrome\" is running then tell application \"Google Chrome\" to count windows",
            ]),
            10.0,
        )?;
        let text = py_strip(&out);
        Ok(if text.is_empty() { 0 } else { eve_vault::pystr::int_of_str(text).map_or(0, |n| n as i64) })
    };
    if windows()? > 0 {
        return Ok(true);
    }
    run(&argv(&["osascript", "-e", "tell application \"Google Chrome\" to make new window"]), 10.0)?;
    sleep(1.0);
    Ok(windows()? > 0)
}

/// the main process, never a Helper
pub const CHROME_BINARY: &str = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";

/// `re.search(r"--user-data-dir=(.+?)(?= --|$)", line)`'s group: the value up to the next " --" or the end.
fn user_data_dir_in(line: &str) -> Option<&str> {
    const FLAG: &str = "--user-data-dir=";
    let mut from = 0;
    while let Some(at) = line[from..].find(FLAG) {
        let start = from + at + FLAG.len();
        let rest = &line[start..];
        if let Some(first) = rest.chars().next() {
            let skip = first.len_utf8();
            let end = rest[skip..].find(" --").map_or(rest.len(), |i| i + skip);
            return Some(&rest[..end]);
        }
        from = start;
    }
    None
}

/// True when the owner's own Chrome (the default user data directory) is running. eve's own Chrome is a second
/// Chrome process with `--user-data-dir=<own dir>`; it does not count, because Codex's browser extension lives
/// only in the owner's profile.
pub fn owner_chrome_running() -> anyhow::Result<bool> {
    owner_chrome_running_with(&run_command)
}

pub fn owner_chrome_running_with(run: Run<'_>) -> anyhow::Result<bool> {
    let out = run(&argv(&["ps", "-axo", "command="]), 10.0)?;
    let default = eve_steel_local::py_path(DEFAULT_CHROME_DIR);
    for line in out.lines() {
        if !line.starts_with(CHROME_BINARY) || line.starts_with(&format!("{CHROME_BINARY} Helper")) {
            continue;
        }
        // the default folder has a space in it
        match user_data_dir_in(line) {
            None => return Ok(true),
            Some(dir) if eve_steel_local::py_path(dir) == default => return Ok(true),
            Some(_) => {}
        }
    }
    Ok(false)
}

/// Start the owner's Chrome when it is not running, so Codex's browser extension has a browser to connect from.
/// Live, 2026-10-07: only eve's own Chrome was open, the chrome lane handed a Turnitin login to Codex, and Codex
/// answered "the browser-control connection is unavailable" in 8 s. `open -a` would only bring the running eve
/// Chrome forward, so this is `open -na`: a new instance on the default profile. True when the owner's Chrome is
/// running afterwards.
pub fn ensure_owner_chrome() -> anyhow::Result<bool> {
    ensure_owner_chrome_with(&run_command, &|s| std::thread::sleep(Duration::from_secs_f64(s)), 10.0, 3.0)
}

pub fn ensure_owner_chrome_with(run: Run<'_>, sleep: &dyn Fn(f64), wait_secs: f64, settle_secs: f64) -> anyhow::Result<bool> {
    if owner_chrome_running_with(run)? {
        return Ok(true);
    }
    tracing::info!("browser lane: the owner's Chrome is not running; starting it for Codex's browser extension");
    run(&argv(&["open", "-na", CHROME_APP]), 10.0)?;
    let deadline = deadline_after(Instant::now(), wait_secs);
    while Instant::now() < deadline {
        sleep(0.5);
        if owner_chrome_running_with(run)? {
            sleep(settle_secs); // the extension's service worker connects after the window
            return Ok(true);
        }
    }
    tracing::warn!("browser lane: the owner's Chrome did not start within {:.0} s", wait_secs);
    Ok(false)
}

/// The owner's Chrome cannot be reached: remote debugging is off, or Chrome is not running.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct AttachError(pub String);

/// Start eve's own Chrome, or hand `url` to it when it is already running (Chrome passes a second launch on the
/// same user data directory to the running one). Port 0: Chrome picks a free port and writes it to
/// `<own_dir>/DevToolsActivePort`, so it never collides with the owner's Chrome.
pub fn own_chrome_command(own_dir: &Path, url: &str) -> Vec<String> {
    let mut cmd = argv(&["open", "-na", CHROME_APP, "--args"]);
    cmd.push(format!("--user-data-dir={}", own_dir.display()));
    cmd.extend(argv(&["--remote-debugging-port=0", "--no-first-run", "--no-default-browser-check"]));
    if !url.is_empty() {
        cmd.push(url.to_string());
    }
    cmd
}

/// Eve's own Chrome with NO debugging port, for the owner to sign in: Google's sign-in may refuse a Chrome that
/// another program can drive (owner, 2026-09-27: "i dont think u can sign into here"). The sign-ins stay in
/// `own_dir`; the next start with the port is already signed in.
pub fn sign_in_command(own_dir: &Path, url: &str) -> Vec<String> {
    let mut cmd = argv(&["open", "-na", CHROME_APP, "--args"]);
    cmd.push(format!("--user-data-dir={}", own_dir.display()));
    cmd.extend(argv(&["--no-first-run", "--no-default-browser-check", url]));
    cmd
}

/// True while any Chrome runs on eve's folder, with the debugging port or without it.
pub fn own_chrome_running(own_dir: &Path) -> anyhow::Result<bool> {
    own_chrome_running_with(own_dir, &run_command)
}

pub fn own_chrome_running_with(own_dir: &Path, run: Run<'_>) -> anyhow::Result<bool> {
    let out = run(
        &[
            "pgrep".to_string(),
            "-f".to_string(),
            "--".to_string(),
            format!("--user-data-dir={}", own_dir.display()),
        ],
        10.0,
    )?;
    Ok(!py_strip(&out).is_empty())
}

/// Quit eve's own Chrome through its own port (Browser.close), never by app name: the owner's Chrome is the same
/// app. True when it is not running afterwards.
pub fn quit_own_chrome(own_dir: &Path, wait_secs: f64) -> anyhow::Result<bool> {
    if let Some(endpoint) = live_endpoint(own_dir) {
        // the connection drops as Chrome quits
        crate::cdp::close_browser_at(&endpoint, Duration::from_millis(10000))?;
    }
    let deadline = deadline_after(Instant::now(), wait_secs);
    while own_chrome_running(own_dir)? {
        if Instant::now() >= deadline {
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Ok(true)
}

/// [`devtools_endpoint`], believed only while something listens on its port: a Chrome that has quit leaves its
/// DevToolsActivePort behind.
pub fn live_endpoint(chrome_dir: &Path) -> Option<String> {
    live_endpoint_with(chrome_dir, &listening)
}

pub fn live_endpoint_with(chrome_dir: &Path, listening: &dyn Fn(i64) -> bool) -> Option<String> {
    let endpoint = devtools_endpoint_with(chrome_dir, DEFAULT_DEBUG_PORT, listening)?;
    let port = endpoint.split(':').nth(2)?.split('/').next()?;
    let port = eve_vault::pystr::int_of_str(port)? as i64;
    listening(port).then_some(endpoint)
}

/// Eve's own Chrome's browser WebSocket, starting that Chrome when it is not running (and handing it `url` when
/// one is given). An [`AttachError`] when it does not come up within [`OWN_START_SECS`].
pub fn start_own_chrome(own_dir: &Path, url: &str) -> anyhow::Result<String> {
    start_own_chrome_with(own_dir, url, &run_command, OWN_START_SECS, 0.25, &listening, &|d| {
        own_chrome_running(d).unwrap_or(false)
    })
}

/// [`start_own_chrome`] with its seams: the subprocess, the waits, the port probe and the running check.
pub fn start_own_chrome_with(
    own_dir: &Path,
    url: &str,
    run: Run<'_>,
    wait_secs: f64,
    poll_secs: f64,
    listening: &dyn Fn(i64) -> bool,
    running: &dyn Fn(&Path) -> bool,
) -> anyhow::Result<String> {
    let endpoint = live_endpoint_with(own_dir, listening);
    if let Some(e) = &endpoint {
        if url.is_empty() {
            return Ok(e.clone());
        }
    }
    if !own_dir.is_dir() {
        if let Some(parent) = own_dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        if let Err(e) = builder.create(own_dir) {
            if !own_dir.is_dir() {
                return Err(e.into());
            }
        }
    }
    if endpoint.is_none() {
        // stale: the new Chrome writes its own
        let _ = std::fs::remove_file(own_dir.join("DevToolsActivePort"));
    }
    run(&own_chrome_command(own_dir, url), 15.0)?;
    let deadline = deadline_after(Instant::now(), wait_secs);
    loop {
        if let Some(endpoint) = live_endpoint_with(own_dir, listening) {
            tracing::info!("browser lane: eve's own Chrome is up ({})", own_dir.display());
            return Ok(endpoint);
        }
        if Instant::now() >= deadline {
            if running(own_dir) {
                return Err(AttachError(
                    "eve's own Chrome is open for signing in (no port): quit it (⌘Q in that Chrome) and eve can use it"
                        .to_string(),
                )
                .into());
            }
            return Err(AttachError(format!(
                "eve's own Chrome did not start within {wait_secs:.0} s ({})",
                own_dir.display()
            ))
            .into());
        }
        std::thread::sleep(Duration::try_from_secs_f64(poll_secs.max(0.0)).unwrap_or_default());
    }
}

// ---- which lane sees a goal ------------------------------------------------------------------------------------

/// Code decides which lane sees a goal; the browser gets a URL, a site, or a named browser.
pub fn is_web_goal(goal: &str, route_kind: &str) -> bool {
    route_kind == "open_url" || route_kind == "search" || WEB_WORDS.is_match(goal)
}

// The Chrome lane drives one browser tab and nothing else, so a web goal that also needs the Mac goes to Codex,
// which has both. "The Mac" is said outright ("on my computer", "the desktop app"), or it is a Mac app named by a
// name no one uses for anything else (Spotify, Photo Booth, Slack) with no browser named for it. Apple's
// everyday-word apps (News, Weather, Maps …) are left out: "the latest news on Google" is a web search.
pub static ON_THE_MAC: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!(
        r"(?i)\b(?:on|in|from) (?:my|the) (?:computer|mac|macbook|laptop|desktop)\b|",
        r"\b(?:desktop|native|mac) (?:app|version)\b"
    ))
});
pub static IN_A_BROWSER: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!(
        r"(?i)(https?://|www\.|\b(?:in|on|using|with|via|through) (?:a |the |my )?(?:web ?)?",
        r"(?:browser|chrome|google chrome|safari|firefox|arc|brave|edge|web|internet)\b|",
        r"\bweb (?:version|app|player)\b)"
    ))
});
pub const BROWSER_APPS: [&str; 7] =
    ["google chrome", "safari", "firefox", "arc", "brave browser", "microsoft edge", "chromium"];
pub const EVERYDAY_WORD_APPS: [&str; 34] = [
    "apps", "books", "calendar", "chess", "clock", "console", "contacts", "dictionary", "freeform", "games", "home",
    "journal", "linear", "magnifier", "mail", "maps", "messages", "music", "news", "notes", "passwords", "phone",
    "photos", "podcasts", "preview", "reminders", "screenshot", "shortcuts", "siri", "stocks", "tips", "tuner", "tv",
    "weather",
];

/// A web goal ([`is_web_goal`]) that the Chrome lane can finish alone: nothing in it is on the Mac.
pub fn is_browser_only_goal(goal: &str, apps: &[String]) -> bool {
    use eve_desktop_lanes::task_router::{app_mentions, explicit_search};
    if !is_web_goal(goal, "") || ON_THE_MAC.is_match(goal) {
        return false;
    }
    if explicit_search(goal) {
        return true; // "google how to reset Spotify": the app is what the search is about
    }
    let native: Vec<String> = app_mentions(goal, apps)
        .into_iter()
        .filter(|a| {
            let folded = casefold(a);
            !BROWSER_APPS.contains(&folded.as_str()) && !EVERYDAY_WORD_APPS.contains(&folded.as_str())
        })
        .collect();
    native.is_empty() || IN_A_BROWSER.is_match(goal)
}

fn has_control(value: &str) -> bool {
    value.chars().any(|c| py_is_space(c) || (c as u32) < 32 || c as u32 == 127)
}

/// A complete observed http(s) origin/path suitable for a planner criterion.
///
/// Query, fragment and credential-bearing links may identify different actions, so omitting those components
/// would invent an ambiguous destination.
pub fn navigation_destination(value: &str) -> String {
    if value.is_empty() || has_control(value) || value.contains('?') || value.contains('#') {
        return String::new();
    }
    let Ok(parsed) = urlsplit(value) else { return String::new() };
    let scheme = parsed.scheme.to_lowercase();
    let Some(host) = parsed.hostname() else { return String::new() };
    let Ok(port) = parsed.port() else { return String::new() };
    if !(scheme == "http" || scheme == "https") || parsed.username().is_some() || parsed.password().is_some() {
        return String::new();
    }
    if port == Some(0) {
        return String::new();
    }
    let mut authority = if host.contains(':') { format!("[{host}]") } else { host };
    if let Some(p) = port {
        if p != if scheme == "https" { 443 } else { 80 } {
            authority.push_str(&format!(":{p}"));
        }
    }
    let path = if parsed.path.is_empty() { "/".to_string() } else { parsed.path.clone() };
    let path = if path.starts_with('/') { path } else { format!("/{path}") };
    format!("{scheme}://{authority}{path}")
}

/// [`navigation_destination`] of any value: "" for anything but a string.
pub fn navigation_destination_value(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => navigation_destination(s),
        _ => String::new(),
    }
}

/// Host-only page context, including sites whose full URL is ambiguous.
pub fn observed_site(value: &str) -> String {
    if has_control(value) {
        return String::new();
    }
    let Ok(parsed) = urlsplit(value) else { return String::new() };
    let scheme = parsed.scheme.to_lowercase();
    if !(scheme == "http" || scheme == "https") || parsed.username().is_some() || parsed.password().is_some() {
        return String::new();
    }
    parsed.hostname().unwrap_or_default()
}

/// [`observed_site`] of any value: "" for anything but a string.
pub fn observed_site_value(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => observed_site(s),
        _ => String::new(),
    }
}

/// What a screenshot paints over: every secret field (as the collector decides it) and every field the vault
/// filled. The mask is painted inside the page, so the pixels never leave the browser unmasked.
pub const SECRET_FIELDS: &str = "input[type=password i], [autocomplete*=\"cc-\" i], [autocomplete*=\"one-time-code\" i], \
[autocomplete*=\"current-password\" i], [autocomplete*=\"new-password\" i], [data-eve-filled]";
pub const MASK_COLOR: &str = "#000000";

// ---- a page as a Snapshot ---------------------------------------------------------------------------------------

fn type_error(message: impl Into<String>) -> anyhow::Error {
    LaneError::err("TypeError", message)
}

/// `int(e.get(key, 0))`.
fn int_field(e: &Map<String, Value>, key: &str) -> anyhow::Result<i64> {
    match e.get(key) {
        None => Ok(0),
        Some(v) => match eve_vault::pystr::int_of_value(v) {
            Some(n) => Ok(n.clamp(i64::MIN as i128, i64::MAX as i128) as i64),
            None => match v {
                Value::String(s) => Err(LaneError::err(
                    "ValueError",
                    format!("invalid literal for int() with base 10: {}", repr(s)),
                )),
                other => Err(type_error(format!(
                    "int() argument must be a string, a bytes-like object or a real number, not '{}'",
                    py_type_name(other)
                ))),
            },
        },
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

/// `raw.get("elements") or []` as rows; a row that is not a mapping is Python's AttributeError on `.get`.
fn element_rows(raw: &Value) -> anyhow::Result<Vec<&Map<String, Value>>> {
    let Some(list) = raw.get("elements").filter(|v| truthy(v)) else {
        return Ok(Vec::new());
    };
    let Value::Array(items) = list else {
        return Err(type_error(format!("'{}' object is not iterable", py_type_name(list))));
    };
    items
        .iter()
        .map(|e| {
            e.as_object()
                .ok_or_else(|| LaneError::err("AttributeError", format!("'{}' object has no attribute 'get'", py_type_name(e))))
        })
        .collect()
}

/// The page's collected elements as the Snapshot the gate and Jev already read.
pub fn snapshot_from_page(raw: &Value, seq: i64, secs: f64) -> anyhow::Result<Snapshot> {
    let rows = element_rows(raw)?;
    let mut elements: Vec<Candidate> = Vec::new();
    let mut focused: Option<Candidate> = None;
    let mut destinations: Vec<String> = Vec::new();
    let mut hrefs: HashMap<String, String> = HashMap::new();
    for e in &rows {
        let raw_role = str_or_empty(e.get("role"));
        let role = role_word(&raw_role)
            .map(str::to_string)
            .unwrap_or_else(|| if raw_role.is_empty() { "control".to_string() } else { raw_role.clone() });
        let id = e.get("id").map(value_str).ok_or_else(|| LaneError::err("KeyError", "'id'"))?;
        let c = Candidate {
            id: id.clone(),
            role,
            label: str_or_empty(e.get("name")),
            value: str_or_empty(e.get("value")),
            frame: (int_field(e, "x")?, int_field(e, "y")?, int_field(e, "w")?, int_field(e, "h")?),
            // AXPress so every role is pressable to the gate; in_content False because on the Mac lane that flag
            // means "web content the AX walk cannot click reliably" — here the page is the whole app and every
            // control is a first-class one.
            actions: vec!["AXPress".to_string()],
            enabled: e.get("enabled").is_none_or(truthy),
            app: "browser".to_string(),
            in_content: false,
            in_dialog: e.get("dialog").is_some_and(truthy),
            secure: e.get("secure").is_some_and(truthy),
            editable: e.get("editable").is_some_and(truthy),
        };
        let destination = navigation_destination_value(e.get("href"));
        if c.role == "link" && !c.label.is_empty() && !e.get("offscreen").is_some_and(truthy) && !destination.is_empty() {
            let item = json!({"id": c.id, "url": destination});
            destinations.push(format!("link destination: {}", eve_steel_local::py_dumps(&item)));
        }
        if e.get("focused").is_some_and(truthy) {
            focused = Some(c.clone());
        }
        // The raw hrefs (query and all: HN's vote?id=…) so the account-change gate sees them.
        if let Some(href) = e.get("href").filter(|h| truthy(h)) {
            hrefs.insert(id, value_str(href));
        }
        elements.push(c);
    }
    elements.sort_by_key(|c| (c.frame.1.div_euclid(8), c.frame.0));
    let title = str_or_empty(raw.get("title"));
    let url_line: String = format!("url: {}", str_or_empty(raw.get("url")).chars().take(120).collect::<String>());
    let mut context: Vec<String> = Vec::new();
    if !title.is_empty() {
        context.push(format!("title: {title}"));
    }
    context.push(url_line);
    context.extend(destinations);
    let n = elements.len();
    Ok(Snapshot {
        seq,
        app: "browser".to_string(),
        bundle: "eve.browser".to_string(),
        title,
        pid: 1,
        elements,
        context_lines: context,
        focused,
        dialog_text: str_or_empty(raw.get("dialog")),
        fullscreen: false,
        node_count: n as i64,
        truncated: n >= MAX_CANDIDATES,
        secs,
        refs: HashMap::new(),
        readouts: Vec::new(),
        hrefs,
    })
}

/// Roles, labels, control states and unambiguous observed link destinations.
pub fn outline_lines(snap: &Snapshot, limit: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut destinations: HashMap<String, String> = HashMap::new();
    for line in &snap.context_lines {
        if let Some(rest) = line.strip_prefix("link destination: ") {
            let Ok(Value::Object(item)) = serde_json::from_str::<Value>(rest) else { continue };
            let destination = navigation_destination_value(item.get("url"));
            if destination.is_empty() {
                continue;
            }
            let Some(id) = item.get("id") else { continue };
            destinations.insert(value_str(id), destination);
        }
    }
    for c in &snap.elements {
        if c.label.is_empty() || c.secure {
            continue;
        }
        let state = if ["selected", "checked", "unchecked", "expanded", "collapsed"].contains(&c.value.as_str()) {
            format!(", {}", c.value)
        } else {
            String::new()
        };
        let destination = match destinations.get(&c.id) {
            Some(d) if c.role == "link" => format!("; destination: {d}"),
            _ => String::new(),
        };
        let label: String = c.label.chars().take(60).collect();
        lines.push(format!("{}: {label}{state}{destination}", c.role));
        if lines.len() >= limit {
            break;
        }
    }
    lines
}

/// `outline_lines`' default limit.
pub const OUTLINE_LIMIT: usize = 80;

// ---- the vault the `fill` step fills from ------------------------------------------------------------------------

/// What the lane asks of the vault (eve_vault::Vault): the lines a planner may see, and the values a fill types.
pub trait LaneVault: Send + Sync {
    /// `vault.lines()`: id, kind, label, site — never a value.
    fn lines(&self) -> Vec<String>;
    /// `vault.fill_values(handle, page_url)`.
    fn fill_values(
        &self,
        handle: &str,
        page_url: &str,
    ) -> Result<(eve_vault::Item, eve_vault::FillValues), eve_vault::VaultError>;
}

impl LaneVault for eve_vault::Vault {
    fn lines(&self) -> Vec<String> {
        eve_vault::Vault::lines(self, eve_vault::LINES_LIMIT)
    }

    fn fill_values(
        &self,
        handle: &str,
        page_url: &str,
    ) -> Result<(eve_vault::Item, eve_vault::FillValues), eve_vault::VaultError> {
        eve_vault::Vault::fill_values(self, handle, page_url, "")
    }
}

// ---- the page's senses and effectors ----------------------------------------------------------------------------

/// A clock in seconds (`time.perf_counter` by default).
pub type ClockFn = Arc<dyn Fn() -> f64 + Send + Sync>;
/// How the lane waits, in seconds.
pub type SleepFn = Arc<dyn Fn(f64) + Send + Sync>;

/// `_page_options`: the page's clock and sleep (the bot-check wait), for tests.
#[derive(Clone, Default)]
pub struct PageOptions {
    pub clock: Option<ClockFn>,
    pub sleep: Option<SleepFn>,
}

/// What a decision made on the last snapshot was made on: each control as collected, in this document.
#[derive(Debug, Clone, Default)]
struct Recorded {
    url: String,
    doc: String,
    /// id → (role, name, href, dom_id)
    controls: HashMap<String, (String, String, String, String)>,
}

/// Senses and effectors over one browser page, on the lane's thread. The methods and their return strings are
/// the contract desktop_helpers' adapter keeps, because run_plan and run_delegate read them.
pub struct Page {
    pub page: Box<dyn PageHandle>,
    clock: ClockFn,
    /// How the lane waits (seconds). None: the page's own wait.
    sleep: Option<SleepFn>,
    /// the URL of a soft check already waited out once, and still up
    stuck_check: RefCell<String>,
    /// eve_vault::Vault, or None: the fill effector refuses
    vault: Option<Arc<dyn LaneVault>>,
    approve: RefCell<Option<String>>,
    seq: Cell<i64>,
    snapshot: RefCell<Option<Snapshot>>,
    observed_url: RefCell<String>,
    hash_before: RefCell<String>,
    hash0: RefCell<String>,
    recorded: RefCell<Recorded>,
    /// the owner line while the last snapshot is a bot check, else ""
    blocked: RefCell<String>,
    webmcp_probe: bool,
    webmcp_seen: RefCell<String>,
}

/// `(url, title, top-of-page text)`: what challenge_kind is asked of one read.
pub type Look = (String, String, String);

impl Page {
    /// `_Page(page)`: the real clock, the page's own wait, no vault, no WebMCP probe.
    pub fn new(page: Box<dyn PageHandle>) -> Page {
        Page {
            page,
            clock: Arc::new(eve_desktop_lanes::perf_counter),
            sleep: None,
            stuck_check: RefCell::new(String::new()),
            vault: None,
            approve: RefCell::new(None),
            seq: Cell::new(0),
            snapshot: RefCell::new(None),
            observed_url: RefCell::new(String::new()),
            hash_before: RefCell::new(String::new()),
            hash0: RefCell::new(String::new()),
            recorded: RefCell::new(Recorded::default()),
            blocked: RefCell::new(String::new()),
            webmcp_probe: false,
            webmcp_seen: RefCell::new(String::new()),
        }
    }

    pub fn with_clock(mut self, clock: ClockFn) -> Page {
        self.clock = clock;
        self
    }

    pub fn with_sleep(mut self, sleep: SleepFn) -> Page {
        self.sleep = Some(sleep);
        self
    }

    pub fn with_vault(mut self, vault: Option<Arc<dyn LaneVault>>) -> Page {
        self.vault = vault;
        self
    }

    pub fn with_webmcp_probe(mut self, on: bool) -> Page {
        self.webmcp_probe = on;
        self
    }

    /// `_Page(page, vault=, webmcp_probe=, **options)`.
    pub fn with_options(self, options: &PageOptions) -> Page {
        let mut p = self;
        if let Some(c) = &options.clock {
            p.clock = c.clone();
        }
        if let Some(s) = &options.sleep {
            p.sleep = Some(s.clone());
        }
        p
    }

    fn now(&self) -> f64 {
        (self.clock)()
    }

    /// `self.sleep(secs)`: the injected sleep, else `page.wait_for_timeout(int(secs * 1000))`.
    pub fn sleep(&self, secs: f64) {
        match &self.sleep {
            Some(s) => s(secs),
            None => self.page.wait_for_timeout((secs * 1000.0) as u64),
        }
    }

    /// `p.approve`: the label the human's yes covers for the next click ("" or None: none).
    pub fn approve(&self) -> Option<String> {
        self.approve.borrow().clone()
    }

    pub fn set_approve(&self, label: Option<&str>) {
        *self.approve.borrow_mut() = label.map(str::to_string);
    }

    /// `p.blocked`: the owner line while the last snapshot is a bot check, else "".
    pub fn blocked(&self) -> String {
        self.blocked.borrow().clone()
    }

    /// The last snapshot (`p._snapshot`), None after a failed read.
    pub fn last_snapshot(&self) -> Option<Snapshot> {
        self.snapshot.borrow().clone()
    }

    // -- senses --

    /// Read the current document, retrying only the navigation context race.
    ///
    /// Chromium can replace the execution context between a click and evaluate. Waiting for the next context is
    /// observation, never another input or a cached answer. Other failures retain their existing error handling.
    pub fn read_dom<T>(&self, mut read: impl FnMut() -> anyhow::Result<T>) -> anyhow::Result<T> {
        let deadline = self.now() + SNAPSHOT_TIMEOUT_MS as f64 / 1000.0;
        let last = SNAPSHOT_TIMEOUT_MS / NAV_READ_POLL_MS;
        for attempt in 0..=last {
            match read() {
                Ok(v) => return Ok(v),
                Err(e) => {
                    let detail = casefold(&error_message(&e));
                    let navigation = detail.contains("execution context was destroyed")
                        || detail.contains("cannot find context with specified id");
                    if !navigation || self.now() >= deadline || attempt == last {
                        return Err(e);
                    }
                    self.page.wait_for_timeout(NAV_READ_POLL_MS);
                }
            }
        }
        // defensive: the loop always returns
        Err(LaneError::err("RuntimeError", "document observation retry limit"))
    }

    fn collect(&self) -> anyhow::Result<Value> {
        let raw = self.read_dom(|| self.page.evaluate(COLLECT_JS, None))?;
        Ok(if raw.is_object() { raw } else { json!({"elements": []}) })
    }

    /// `read()` again while what it read is a soft bot check (challenge_kind "soft"), every CHALLENGE_POLL_SECS
    /// for at most CHALLENGE_SETTLE_SECS, and return the last read. `look` turns a read into (url, title, text).
    /// A hard check or an ordinary page returns at once. A soft check that was already waited out on this URL is
    /// not waited on again: the plan is stopping on it.
    pub fn settle_check<T>(
        &self,
        mut read: impl FnMut() -> anyhow::Result<T>,
        look: impl Fn(&T) -> anyhow::Result<Look>,
    ) -> anyhow::Result<T> {
        let mut value = read()?;
        let (url, title, text) = look(&value)?;
        if challenge_kind(&url, &title, &text) != "soft" {
            self.stuck_check.borrow_mut().clear();
            return Ok(value);
        }
        if !url.is_empty() && url == *self.stuck_check.borrow() {
            return Ok(value);
        }
        let mut last_url = url;
        let t0 = self.now();
        // bounded if the clock stands still
        let rounds = (CHALLENGE_SETTLE_SECS / CHALLENGE_POLL_SECS.max(0.05)) as usize;
        for _ in 0..rounds {
            if self.now() - t0 + CHALLENGE_POLL_SECS > CHALLENGE_SETTLE_SECS {
                break;
            }
            self.sleep(CHALLENGE_POLL_SECS);
            value = read()?;
            let (url, title, text) = look(&value)?;
            last_url = url;
            if challenge_kind(&last_url, &title, &text) != "soft" {
                tracing::info!("browser lane: a bot check cleared by itself after {:.1} s", self.now() - t0);
                self.stuck_check.borrow_mut().clear();
                return Ok(value);
            }
        }
        tracing::info!("browser lane: a bot check was still up after {:.1} s", self.now() - t0);
        *self.stuck_check.borrow_mut() = last_url;
        Ok(value)
    }

    /// (url, title, top-of-page text) from one `_COLLECT_JS` read: what challenge_kind is asked.
    pub fn raw_look(raw: &Value) -> Look {
        let head = match raw.get("head") {
            Some(Value::String(h)) => h.clone(),
            _ => {
                let hash = str_or_empty(raw.get("text_hash"));
                hash.split_once(':').map(|(_, rest)| rest.to_string()).unwrap_or_default()
            }
        };
        let url = match raw.get("url") {
            Some(Value::String(u)) => u.clone(),
            _ => String::new(),
        };
        (url, str_or_empty(raw.get("title")), head)
    }

    pub fn snapshot(&self) -> anyhow::Result<Snapshot> {
        self.seq.set(self.seq.get() + 1);
        *self.snapshot.borrow_mut() = None; // a failed read must not leave focused() using the previous document
        self.observed_url.borrow_mut().clear();
        *self.recorded.borrow_mut() = Recorded::default();
        let t0 = self.now();
        // a Cloudflare check that clears is waited out
        let raw = self.settle_check(|| self.collect(), |r| Ok(Self::raw_look(r)))?;
        let snap = snapshot_from_page(&raw, self.seq.get(), self.now() - t0)?;
        *self.snapshot.borrow_mut() = Some(snap.clone());
        let observed = match raw.get("url") {
            Some(Value::String(u)) => u.clone(),
            _ => String::new(),
        };
        *self.observed_url.borrow_mut() = observed.clone();
        let rows = element_rows(&raw)?;
        // What a decision made on this snapshot is made on: each control as collected, in this document.
        *self.recorded.borrow_mut() = Recorded {
            url: observed.clone(),
            doc: str_or_empty(raw.get("document_id")),
            controls: rows
                .iter()
                .map(|e| {
                    let id = e.get("id").map_or_else(|| "None".to_string(), value_str);
                    (
                        id,
                        (
                            str_or_empty(e.get("role")),
                            str_or_empty(e.get("name")),
                            str_or_empty(e.get("href")),
                            str_or_empty(e.get("dom_id")),
                        ),
                    )
                })
                .collect(),
        };
        let (u, t, h) = Self::raw_look(&raw);
        *self.blocked.borrow_mut() =
            if browser_blocker(&u, &t, &h).is_empty() { String::new() } else { blocked_line(&observed) };
        // What "changed" means here: the URL, the title, the page text, or any control's state or label. A toggled
        // checkbox changes no text, so the controls are in the hash too.
        let controls: Vec<Value> = rows
            .iter()
            .map(|e| {
                let g = |k: &str| e.get(k).cloned().unwrap_or(Value::Null);
                json!([g("role"), g("name"), g("value")])
            })
            .collect();
        let g = |k: &str| raw.get(k).cloned().unwrap_or(Value::Null);
        let doc = Value::Array(vec![g("url"), g("title"), g("text_hash"), Value::Array(controls)]);
        let digest = Sha1::digest(eve_steel_local::py_dumps(&doc).as_bytes());
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        let previous = std::mem::replace(&mut *self.hash0.borrow_mut(), hex);
        *self.hash_before.borrow_mut() = previous;
        if self.webmcp_probe {
            self.probe_webmcp(&observed)?;
        }
        Ok(snap)
    }

    /// Log, once per page, whether it offers WebMCP tools. Only the host and the tool names are logged, never the
    /// path or query. A failed probe is silent: it must never cost a step.
    fn probe_webmcp(&self, url: &str) -> anyhow::Result<()> {
        let parts = urlsplit(url).map_err(|e| LaneError::err("ValueError", e.0))?;
        let Some(host) = parts.hostname().filter(|_| parts.scheme == "http" || parts.scheme == "https") else {
            return Ok(());
        };
        let page_key = format!("{}://{}{}", parts.scheme, parts.netloc, parts.path);
        if page_key == *self.webmcp_seen.borrow() {
            return Ok(());
        }
        *self.webmcp_seen.borrow_mut() = page_key;
        let Ok(found) = self.page.evaluate(WEBMCP_JS, None) else {
            return Ok(()); // measurement only
        };
        let Value::Object(found) = found else { return Ok(()) };
        static UNSAFE: LazyLock<Regex> = LazyLock::new(|| re(r"[^A-Za-z0-9_.-]"));
        let names: Option<Vec<String>> = match found.get("names") {
            Some(Value::Array(list)) => Some(
                list.iter().map(|n| UNSAFE.replace_all(&value_str(n), "").chars().take(64).collect()).collect(),
            ),
            _ => None,
        };
        let api = if found.get("api").is_some_and(truthy) { "True" } else { "False" };
        let tools = names.as_ref().map_or_else(|| "-".to_string(), |n| n.len().to_string());
        let shown = match &names {
            Some(n) if !n.is_empty() => n.join(","),
            _ => "-".to_string(),
        };
        tracing::info!("browser lane: webmcp probe host={} api={} tools={} names={}", host, api, tools, shown);
        Ok(())
    }

    /// Full URL from the last successful snapshot; never perform another read.
    pub fn current_url(&self) -> String {
        self.observed_url.borrow().clone()
    }

    pub fn text_visible(&self, s: &str) -> bool {
        let needle = casefold(&py_collapse(s));
        if needle.is_empty() {
            return false;
        }
        // The page text, the title, and what is typed into its fields (innerText has no field values).
        let read = || -> anyhow::Result<bool> {
            let body = self.page.evaluate(
                "() => (document.body ? document.body.innerText : '') + ' ' + Array.from(document.querySelectorAll('input,textarea')).map(e => e.value || '').join(' ')",
                None,
            )?;
            let body = str_or_empty(Some(&body));
            if casefold(&py_collapse(&body)).contains(&needle) {
                return Ok(true);
            }
            Ok(casefold(&py_collapse(&self.page.title()?)).contains(&needle))
        };
        // a page mid-navigation has no text yet
        self.read_dom(read).unwrap_or(false)
    }

    pub fn focused(&self) -> anyhow::Result<Option<Candidate>> {
        if let Some(snap) = self.snapshot.borrow().as_ref() {
            return Ok(snap.focused.clone());
        }
        Ok(self.snapshot()?.focused)
    }

    pub fn frontmost_pid(&self) -> i64 {
        1 // one page; the executor's focus check never fires
    }

    pub fn screen_changed(&self) -> Option<bool> {
        let (before, now) = (self.hash_before.borrow(), self.hash0.borrow());
        if before.is_empty() || now.is_empty() {
            return None;
        }
        Some(*before != *now)
    }

    // -- effectors --

    fn locator(c: &Candidate) -> String {
        format!("[data-eve-id=\"{}\"]", c.id)
    }

    /// The effectors' refusal on a bot-check page (browser_blocker), "" otherwise.
    fn blocked_refusal(&self) -> String {
        let blocked = self.blocked.borrow();
        if blocked.is_empty() {
            String::new()
        } else {
            format!("{BLOCKED_PREFIX} {blocked}")
        }
    }

    /// "" when the control under `c.id` is still the one the last snapshot recorded, in the same document
    /// (click_recheck); else the STALE_PREFIX refusal, saying what changed.
    fn recheck(&self, c: &Candidate) -> String {
        let (recorded, before) = {
            let rec = self.recorded.borrow();
            let Some(before) = rec.controls.get(&c.id).cloned() else {
                return format!("{STALE_PREFIX} {} was not in the last look at the page; look again", repr(&c.label));
            };
            (
                json!({"url": rec.url, "doc": rec.doc, "role": before.0, "name": before.1, "href": before.2,
                       "dom_id": before.3}),
                before,
            )
        };
        let _ = before;
        let now = match self.page.evaluate(RECHECK_JS, Some(&json!({"id": c.id}))) {
            Ok(v) => v,
            // a document being replaced cannot vouch for the control
            Err(e) => {
                return format!("{STALE_PREFIX} could not re-read {} ({}); look again", repr(&c.label), error_kind(&e))
            }
        };
        let changed = click_recheck(&recorded, &now);
        if !changed.is_empty() {
            return format!(
                "{STALE_PREFIX} {} changed since the decision ({}); look again",
                repr(&c.label),
                changed.join(", ")
            );
        }
        String::new()
    }

    pub fn click_candidate(&self, c: &Candidate) -> String {
        let blocked = self.blocked_refusal();
        if !blocked.is_empty() {
            return blocked;
        }
        let norm = casefold(&py_collapse(&c.label));
        // The raw href (query and all: HN's vote?id=…) from the last look; an upvote or follow link asks too.
        let href = self.recorded.borrow().controls.get(&c.id).map(|r| r.2.clone()).unwrap_or_default();
        let approved = self.approve.borrow().as_deref().is_some_and(|a| !a.is_empty() && casefold(a) == norm); // "" is no yes
        if needs_confirmation(c, &href) && !approved {
            return format!("refused: {} is a sensitive control without approval", repr(&c.label));
        }
        let loc = Self::locator(c);
        let attempt = || -> anyhow::Result<Option<String>> {
            if self.page.count(&loc)? == 0 {
                return Ok(Some(format!("refused: {} is no longer on the page", repr(&c.label))));
            }
            self.page.scroll_into_view_if_needed(&loc, SNAPSHOT_TIMEOUT_MS)?;
            // Read again what the decision was made on, after the scroll (which can lazy-load and re-render) and as
            // the last thing before the press. A separate evaluate, not a click inside it: the click is a real
            // pointer event and waits for the control to be visible, stable and uncovered.
            let stale = self.recheck(c);
            if !stale.is_empty() {
                tracing::info!("browser lane: {}", stale);
                return Ok(Some(stale));
            }
            self.page.click(&loc, SNAPSHOT_TIMEOUT_MS)?;
            Ok(None)
        };
        match attempt() {
            Ok(Some(refusal)) => return refusal,
            Ok(None) => {}
            // covered, detached, navigated away: nothing was clicked
            Err(e) => return format!("refused: could not click {} ({})", repr(&c.label), error_kind(&e)),
        }
        let (x, y, w, h) = c.frame;
        format!("clicked {} {} at ({},{})", c.role, repr(&c.label), x + w.div_euclid(2), y + h.div_euclid(2))
    }

    pub fn focus_and_type(&self, c: &Candidate, text: &str) -> String {
        let blocked = self.blocked_refusal();
        if !blocked.is_empty() {
            return blocked;
        }
        if c.secure {
            return format!("refused: {} is a secure field", repr(&c.label));
        }
        let loc = Self::locator(c);
        let attempt = || -> anyhow::Result<Option<String>> {
            if self.page.count(&loc)? == 0 {
                return Ok(Some(format!("refused: {} is no longer on the page", repr(&c.label))));
            }
            self.page.scroll_into_view_if_needed(&loc, SNAPSHOT_TIMEOUT_MS)?; // a field below the fold
            self.page.click(&loc, SNAPSHOT_TIMEOUT_MS)?;
            let focused = self.page.evaluate(
                "() => document.activeElement && document.activeElement.getAttribute('data-eve-id')",
                None,
            )?;
            if value_str(&focused) != c.id {
                return Ok(Some(format!("refused: focus is not on the {} {}", c.role, repr(&c.label))));
            }
            self.page.keyboard_type(text, 5)?;
            Ok(None)
        };
        match attempt() {
            Ok(Some(refusal)) => return refusal,
            Ok(None) => {}
            Err(e) => return format!("refused: could not type into {} ({})", repr(&c.label), error_kind(&e)),
        }
        format!("typed {} characters into {} {}", text.chars().count(), c.role, repr(&c.label))
    }

    /// Fill the page's form from a vault item: code finds the fields (`_FILL_FIELDS_JS`) and types the values; the
    /// result names the parts filled, never a value. A login or 2FA item fills only on its own site
    /// (eve_vault::Vault::fill_values). "refused: ..." means nothing was typed.
    pub fn fill_from_vault(&self, handle: &str) -> anyhow::Result<String> {
        let blocked = self.blocked_refusal();
        if !blocked.is_empty() {
            return Ok(blocked);
        }
        let Some(vault) = &self.vault else {
            return Ok("refused: there is no vault".to_string());
        };
        let (item, mut values) = match vault.fill_values(handle, &self.page.url()) {
            Ok(found) => found,
            // Python lets an OSError from the Keychain escape; only VaultError is a refusal.
            Err(e @ eve_vault::VaultError::Os(_)) => return Err(LaneError::err("OSError", e.to_string())),
            Err(e) => return Ok(format!("refused: {e}")),
        };
        let found = match self.read_dom(|| self.page.evaluate(FILL_FIELDS_JS, Some(&json!(item.kind)))) {
            Ok(v) => v,
            Err(e) => return Ok(format!("refused: could not read the form ({})", error_kind(&e))),
        };
        let mut todo: Vec<(String, String, String)> = Vec::new();
        if truthy(&found) {
            let Value::Array(rows) = &found else {
                return Err(type_error(format!("'{}' object is not iterable", py_type_name(&found))));
            };
            for row in rows {
                let parts = match row {
                    Value::Array(p) if p.len() == 3 => p,
                    Value::Array(p) if p.len() > 3 => {
                        return Err(LaneError::err("ValueError", "too many values to unpack (expected 3)"))
                    }
                    Value::Array(p) => {
                        return Err(LaneError::err(
                            "ValueError",
                            format!("not enough values to unpack (expected 3, got {})", p.len()),
                        ))
                    }
                    other => {
                        return Err(type_error(format!("cannot unpack non-iterable {} object", py_type_name(other))))
                    }
                };
                let (fid, part, tag) = (value_str(&parts[0]), value_str(&parts[1]), value_str(&parts[2]));
                let base = part.split('#').next().unwrap_or("").to_string();
                if values.contains_key(&base) {
                    todo.push((fid, part, tag));
                }
            }
        }
        if todo.is_empty() {
            values.clear();
            return Ok(format!("refused: no field on this page for the {} {}", item.kind, repr(&item.label)));
        }
        let mut filled: Vec<String> = Vec::new();
        let mut failed: Option<(String, anyhow::Error)> = None;
        for (fid, part, tag) in &todo {
            let (base, index) = match part.split_once('#') {
                Some((b, i)) => (b.to_string(), i.to_string()),
                None => (part.clone(), String::new()),
            };
            let step = || -> anyhow::Result<()> {
                let whole = values.get(&base).cloned().unwrap_or_default();
                let value = if index.is_empty() {
                    whole
                } else {
                    let i = eve_vault::pystr::int_of_str(&index).ok_or_else(|| {
                        LaneError::err("ValueError", format!("invalid literal for int() with base 10: {}", repr(&index)))
                    })?;
                    // Python's values[base][i:i + 1], negative bounds counted from the end
                    let chars: Vec<char> = whole.chars().collect();
                    let n = chars.len() as i128;
                    let bound = |b: i128| if b < 0 { (n + b).max(0) } else { b.min(n) } as usize;
                    let (start, end) = (bound(i), bound(i + 1));
                    if start < end { chars[start..end].iter().collect() } else { String::new() }
                };
                let loc = format!("[data-eve-fill=\"{fid}\"]");
                if tag == "select" {
                    let stripped = value.trim_start_matches('0').to_string();
                    let tail: String = {
                        let chars: Vec<char> = value.chars().collect();
                        chars[chars.len().saturating_sub(2)..].iter().collect()
                    };
                    let mut choices: Vec<String> = Vec::new();
                    for c in [value.clone(), stripped, tail] {
                        if !choices.contains(&c) {
                            choices.push(c);
                        }
                    }
                    if !choices.iter().any(|c| self.select(&loc, c)) {
                        return Err(LaneError::err("ValueError", format!("no {base} option")));
                    }
                } else {
                    self.page.fill(&loc, &value, SNAPSHOT_TIMEOUT_MS)?;
                }
                self.page.evaluate_on(&loc, "e => e.setAttribute('data-eve-filled', '')")?;
                Ok(())
            };
            match step() {
                Ok(()) => {
                    if !filled.contains(&base) {
                        filled.push(base);
                    }
                }
                // a field that would not take the value
                Err(e) => {
                    failed = Some((base, e));
                    break;
                }
            }
        }
        values.clear();
        if let Some((base, e)) = failed {
            let what = format!("could not fill the {base} ({})", error_kind(&e));
            return Ok(if filled.is_empty() {
                format!("refused: {what}")
            } else {
                format!("filled {} from {} but {what}", filled.join(", "), repr(&item.label))
            });
        }
        let ids: Vec<Value> = todo.iter().map(|(fid, _, _)| json!(fid)).collect();
        let empty = self.page.evaluate(
            "ids => ids.filter(i => { const e = document.querySelector(`[data-eve-fill=\"${i}\"]`); return !e || !e.value; }).length",
            Some(&Value::Array(ids)),
        )?;
        if truthy(&empty) {
            return Ok(format!(
                "filled {} from {} but {} field(s) stayed empty",
                filled.join(", "),
                repr(&item.label),
                value_str(&empty)
            ));
        }
        Ok(format!("filled {} from {}", filled.join(", "), repr(&item.label)))
    }

    fn select(&self, loc: &str, choice: &str) -> bool {
        // not this spelling: the next one
        [SelectBy::Value(choice), SelectBy::Label(choice)].into_iter().any(|how| self.page.select_option(loc, how, 1000).is_ok())
    }

    /// Dismiss a cookie/consent notice (CONSENT_TEXT) with a refusing or closing button. The line says what was
    /// pressed, "" when there was no such notice or no safe button. Never fails.
    pub fn clear_consent(&self) -> String {
        let Ok(found) = self.page.evaluate(CONSENT_JS, Some(&json!(CONSENT_TEXT_PATTERN))) else {
            return String::new(); // a page mid-navigation: nothing to dismiss yet
        };
        let Value::Object(found) = found else { return String::new() };
        let buttons = match found.get("buttons") {
            Some(Value::Array(b)) => b.clone(),
            _ => Vec::new(),
        };
        let Some(choice) = consent_choice(&buttons) else { return String::new() };
        let press = || -> anyhow::Result<()> {
            let id = choice.get("id").ok_or_else(|| LaneError::err("KeyError", "'id'"))?;
            self.page.click(&format!("[data-eve-consent=\"{}\"]", value_str(id)), SNAPSHOT_TIMEOUT_MS)
        };
        if let Err(e) = press() {
            return format!("refused: could not dismiss the cookie notice ({})", error_kind(&e));
        }
        self.settle(0.3);
        format!("dismissed the cookie notice with {}", value_repr(choice.get("name").unwrap_or(&Value::Null)))
    }

    pub fn press(&self, key: &str) -> anyhow::Result<String> {
        let Some(name) = key_name(&key.to_lowercase()) else {
            return Err(LaneError::err("ValueError", format!("not a key: {}", repr(key))));
        };
        let blocked = self.blocked_refusal();
        if !blocked.is_empty() {
            return Ok(blocked);
        }
        let before = self.page.url();
        self.page.keyboard_press(name)?;
        if name == "Enter" {
            // A form submitted with Return navigates a beat after the key, and until the new page commits every
            // load-state wait answers for the OLD page: the executor moved on while the results were still on
            // their way (live probe of the eval's docs search, 2026-09-24). Wait for the URL to change, briefly.
            let t0 = self.now();
            while self.now() - t0 < ENTER_NAV_SECS {
                self.page.wait_for_timeout(50);
                if self.page.url() != before {
                    // a slow page is still the new page
                    let _ = self.page.wait_for_load_state("domcontentloaded", NAV_TIMEOUT_MS);
                    break;
                }
            }
        }
        Ok(format!("pressed {key}"))
    }

    pub fn settle(&self, secs: f64) -> f64 {
        let t0 = self.now();
        let cap = (secs.min(SETTLE_CAP_SECS) * 1000.0) as u64;
        // a page that never goes idle is still settled enough
        if self.page.wait_for_load_state("domcontentloaded", cap).is_ok() {
            let _ = self.page.wait_for_load_state("networkidle", cap);
        }
        self.now() - t0
    }

    pub fn open_url(&self, url: &str) -> anyhow::Result<String> {
        static SCHEME: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^(https?|file)://"));
        let url = if SCHEME.is_match(url) { url.to_string() } else { format!("https://{url}") };
        self.page.goto(&url, "domcontentloaded", NAV_TIMEOUT_MS)?;
        Ok(format!("opened {url}"))
    }
}

impl Senses for Page {
    fn snapshot(&self) -> anyhow::Result<Snapshot> {
        Page::snapshot(self)
    }

    fn text_visible(&self, s: &str) -> anyhow::Result<bool> {
        Ok(Page::text_visible(self, s))
    }

    fn focused(&self) -> Option<Candidate> {
        Page::focused(self).ok().flatten()
    }

    fn frontmost_pid(&self) -> anyhow::Result<i64> {
        Ok(Page::frontmost_pid(self))
    }

    fn screen_changed(&self) -> anyhow::Result<Option<bool>> {
        Ok(Page::screen_changed(self))
    }

    fn current_url(&self) -> Option<anyhow::Result<String>> {
        Some(Ok(Page::current_url(self)))
    }
}

impl Effectors for Page {
    fn click_candidate(&self, c: &Candidate) -> anyhow::Result<String> {
        Ok(Page::click_candidate(self, c))
    }

    fn focus_and_type(&self, c: &Candidate, text: &str) -> anyhow::Result<String> {
        Ok(Page::focus_and_type(self, c, text))
    }

    fn press(&self, key: &str) -> anyhow::Result<String> {
        Page::press(self, key)
    }

    fn settle(&self, secs: f64) -> anyhow::Result<f64> {
        Ok(Page::settle(self, secs))
    }

    fn fill_from_vault(&self, handle: &str) -> Option<anyhow::Result<String>> {
        Some(Page::fill_from_vault(self, handle))
    }

    fn clear_consent(&self) -> Option<anyhow::Result<String>> {
        Some(Ok(Page::clear_consent(self)))
    }

    fn grant(&self, approved: &str) {
        self.set_approve(Some(approved));
    }
}

// ---- the lane --------------------------------------------------------------------------------------------------

/// What [`LaneCore::outline`] hands the planner: the page as lines, its address and site, the vault's lines, and
/// the owner line when a bot check is up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Outline {
    pub app: String,
    pub lines: Vec<String>,
    pub title: String,
    pub url: String,
    pub site: String,
    pub vault: Vec<String>,
    /// a bot check: the owner line; the caller says it and stops
    pub blocked: String,
}

/// What [`LaneCore::page_text`] reads: the page's title, URL and visible text (whitespace-collapsed, capped).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PageText {
    pub title: String,
    pub url: String,
    pub text: String,
    pub signed_out: bool,
    pub blocked: String,
}

/// The test seam that hands the lane a context instead of a browser (`launcher=`).
pub type Launcher = Box<dyn FnMut() -> anyhow::Result<Arc<dyn ContextHandle>> + Send>;

/// `devtools_endpoint(chrome_dir, port)` as a seam.
pub type EndpointFn = Arc<dyn Fn(&Path, i64) -> Option<String> + Send + Sync>;
/// `start_own_chrome(own_dir)` as a seam.
pub type OwnChromeFn = Arc<dyn Fn(&Path) -> anyhow::Result<String> + Send + Sync>;
/// `_fetch_accounts(url, cookie header, timeout)` as a seam.
pub type FetchAccountsFn = Arc<dyn Fn(&str, &str, f64) -> anyhow::Result<String> + Send + Sync>;
/// `_profiles()` as a seam.
pub type ProfilesFn = Arc<dyn Fn() -> IndexMap<String, Arc<dyn ContextHandle>> + Send + Sync>;

/// The module functions the Python tests monkeypatched, as values: the driver start, the endpoint reading, the
/// window check, eve's own Chrome, Steel, Google's account list, and `_profiles`.
#[derive(Clone)]
pub struct LaneSeams {
    pub start_driver: StartDriver,
    pub devtools_endpoint: EndpointFn,
    pub ensure_chrome_window: Arc<dyn Fn() -> anyhow::Result<bool> + Send + Sync>,
    pub start_own_chrome: OwnChromeFn,
    pub steel_start: Arc<dyn Fn() -> anyhow::Result<String> + Send + Sync>,
    pub steel_release: Arc<dyn Fn() + Send + Sync>,
    /// `_fetch_accounts`: (url, cookie header, timeout) → the reply body
    pub fetch_accounts: FetchAccountsFn,
    /// replaces `_profiles` (tests only)
    pub profiles: Option<ProfilesFn>,
    /// PROFILE_LOOKUP_BUDGET_SECS (the tests shorten it)
    pub profile_lookup_budget_secs: f64,
}

/// Run a future to completion on a runtime of its own, on a thread of its own (callable from any thread,
/// including one inside another runtime).
fn block_on<F>(f: F) -> anyhow::Result<F::Output>
where
    F: Future + Send,
    F::Output: Send,
{
    std::thread::scope(|s| {
        s.spawn(|| -> anyhow::Result<F::Output> {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
            Ok(rt.block_on(f))
        })
        .join()
        .map_err(|_| anyhow::anyhow!("the runtime thread panicked"))?
    })
}

impl Default for LaneSeams {
    fn default() -> Self {
        LaneSeams {
            start_driver: Arc::new(crate::cdp::CdpDriver::start),
            devtools_endpoint: Arc::new(devtools_endpoint),
            ensure_chrome_window: Arc::new(ensure_chrome_window),
            start_own_chrome: Arc::new(|dir| start_own_chrome(dir, "")),
            steel_start: Arc::new(|| {
                block_on(eve_steel_local::start_session(&eve_steel_local::configured(), false))?
            }),
            steel_release: Arc::new(|| {
                let _ = block_on(eve_steel_local::release(&eve_steel_local::configured()));
            }),
            fetch_accounts: Arc::new(fetch_accounts),
            profiles: None,
            profile_lookup_budget_secs: PROFILE_LOOKUP_BUDGET_SECS,
        }
    }
}

/// `BrowserLane(config, step_asker=, launcher=, vault=)`'s keyword arguments.
#[derive(Default)]
pub struct LaneOptions {
    pub step_asker: Option<Box<dyn StepAsk + Send>>,
    pub launcher: Option<Launcher>,
    /// what the `fill` step fills from; None opens the default vault
    pub vault: Option<Arc<dyn LaneVault>>,
}

/// The lane's state and its synchronous jobs: everything that runs on the lane's one thread.
pub struct LaneCore {
    pub config: BrowserLaneConfig,
    /// what the `fill` step fills from (eve_vault)
    pub vault: Option<Arc<dyn LaneVault>>,
    step_asker: Option<Box<dyn StepAsk + Send>>,
    launcher: Option<Launcher>,
    pub pw: Option<Box<dyn Driver>>,
    pub context: Option<Arc<dyn ContextHandle>>,
    pub page: Option<Page>,
    /// tests only: an https URL the plan contract insists on → a loopback http one
    pub plain_http: HashMap<String, String>,
    /// tests only: the page clock and sleep (the bot-check wait)
    pub page_options: PageOptions,
    /// attach mode: the owner's Chrome, connected over CDP
    pub browser: Option<Arc<dyn BrowserHandle>>,
    /// attach mode: {account: context}, per connection
    pub profile_map: Option<IndexMap<String, Arc<dyn ContextHandle>>>,
    pub seams: LaneSeams,
}

/// `profiles.get(email)` by identity-preserving clone.
fn lookup(profiles: &IndexMap<String, Arc<dyn ContextHandle>>, email: &str) -> Option<Arc<dyn ContextHandle>> {
    profiles.get(email).cloned()
}

fn index_error() -> anyhow::Error {
    LaneError::err("IndexError", "list index out of range")
}

impl LaneCore {
    /// The lane with no browser yet. `options.vault` None opens the default vault (Python's `Vault()`).
    pub fn new(config: BrowserLaneConfig, options: LaneOptions) -> anyhow::Result<LaneCore> {
        let vault: Arc<dyn LaneVault> = match options.vault {
            Some(v) => v,
            None => Arc::new(eve_vault::Vault::open_default()?),
        };
        Ok(LaneCore {
            config,
            vault: Some(vault),
            step_asker: options.step_asker,
            launcher: options.launcher,
            pw: None,
            context: None,
            page: None,
            plain_http: HashMap::new(),
            page_options: PageOptions::default(),
            browser: None,
            profile_map: None,
            seams: LaneSeams::default(),
        })
    }

    fn make_page(&self, page: Box<dyn PageHandle>) -> Page {
        Page::new(page).with_vault(self.vault.clone()).with_webmcp_probe(true).with_options(&self.page_options)
    }

    /// The page the lane works in, connecting or launching first when needed.
    pub fn ensure(&mut self) -> anyhow::Result<&Page> {
        if let Some(p) = &self.page {
            if p.page.title().is_ok() {
                // alive
                return Ok(self.page.as_ref().expect("checked above"));
            }
            self.page = None; // the owner closed the window: open a fresh one
        }
        if self.context.is_none() && self.config.attach && self.launcher.is_none() {
            let endpoint = if self.config.steel {
                // no key, no dialog, no window
                match (self.seams.steel_start)() {
                    Ok(e) => e,
                    Err(e) => match e.downcast_ref::<eve_steel_local::SteelError>() {
                        Some(se) => return Err(AttachError(se.0.clone()).into()),
                        None => return Err(e),
                    },
                }
            } else if self.config.own {
                (self.seams.start_own_chrome)(&self.config.own_dir)? // eve's Chrome: no dialog, no window needed
            } else {
                let Some(endpoint) = (self.seams.devtools_endpoint)(&self.config.chrome_dir, self.config.debug_port) else {
                    return Err(AttachError(
                        "Chrome's remote debugging is off (or Chrome is not running): open \
                         chrome://inspect/#remote-debugging in Chrome and switch it on"
                            .to_string(),
                    )
                    .into());
                };
                (self.seams.ensure_chrome_window)()?; // no window: Chrome answers 403 instead of asking
                endpoint
            };
            let mut pw = (self.seams.start_driver)()?;
            let browser = match pw.connect_over_cdp(&endpoint, CONSENT_TIMEOUT_MS) {
                Ok(b) => b,
                Err(e) => {
                    let _ = pw.stop();
                    if e.to_string().contains("403") || error_message(&e).contains("403") {
                        return Err(AttachError(
                            "Chrome refused the connection (403): it was declined, or Chrome had no window to ask in"
                                .to_string(),
                        )
                        .into());
                    }
                    return Err(e);
                }
            };
            self.pw = Some(pw);
            self.browser = Some(browser);
            let wanted = self.config.chrome_profile.clone();
            let opened = (|| -> anyhow::Result<Page> {
                let context = self.pick_profile(&wanted)?; // the owner's profile: their logins
                self.context = Some(context.clone());
                let page = context.new_page()?; // eve's own tab; the owner's tabs are never touched
                page.bring_to_front()?;
                Ok(self.make_page(page))
            })();
            return match opened {
                Ok(page) => {
                    self.page = Some(page);
                    Ok(self.page.as_ref().expect("just set"))
                }
                Err(e) => {
                    // No profile window (AttachError) or no tab: disconnect and stop the driver, or the connection
                    // leaks and the next ensure starts a second driver beside it.
                    self.close();
                    Err(e)
                }
            };
        }
        if self.context.is_none() {
            if let Some(launcher) = self.launcher.as_mut() {
                self.context = Some(launcher()?);
            } else {
                let mut pw = (self.seams.start_driver)()?;
                std::fs::create_dir_all(&self.config.profile)?;
                let context = pw.launch_persistent_context(
                    &self.config.profile,
                    self.config.headless,
                    DEFAULT_VIEWPORT,
                    &["--disable-blink-features=AutomationControlled"],
                );
                self.pw = Some(pw);
                self.context = Some(context?);
            }
        }
        let context = self.context.clone().expect("set above");
        let page = if self.browser.is_some() {
            // Attached, and eve's tab is gone (the owner closed it, or Chrome dropped it): a NEW tab of its own.
            // Never pages[0] — in the owner's Chrome that is one of THEIR tabs.
            context.new_page()?
        } else {
            let mut pages = context.pages()?;
            if pages.is_empty() {
                context.new_page()?
            } else {
                pages.remove(0)
            }
        };
        page.bring_to_front()?;
        self.page = Some(self.make_page(page));
        Ok(self.page.as_ref().expect("just set"))
    }

    /// {primary account email: its Chrome context} for every profile with an open window. Nothing is opened on
    /// screen. Cached for the connection's life ([`LaneCore::close`] forgets it).
    ///
    /// Every profile is asked at once, within PROFILE_LOOKUP_BUDGET_SECS in total. Each context's cookies for
    /// Google's account list are read here (a local CDP call), and the requests themselves run on short-lived
    /// threads with plain HTTP. A profile that has not answered within the budget is simply not addressable by
    /// name.
    pub fn profiles(&mut self) -> anyhow::Result<IndexMap<String, Arc<dyn ContextHandle>>> {
        if let Some(over) = &self.seams.profiles {
            return Ok(over());
        }
        if let Some(found) = &self.profile_map {
            return Ok(found.clone());
        }
        let t0 = Instant::now();
        let contexts = match &self.browser {
            Some(b) => b.contexts()?,
            None => Vec::new(),
        };
        let mut jobs: Vec<(Arc<dyn ContextHandle>, String)> = Vec::new();
        for ctx in &contexts {
            // a profile that cannot say is simply not addressable by name
            let Ok(cookies) = ctx.cookies(&[ACCOUNTS_URL]) else { continue };
            let header: Vec<String> =
                cookies.iter().filter(|c| !c.name.is_empty()).map(|c| format!("{}={}", c.name, c.value)).collect();
            let header = header.join("; ");
            if !header.is_empty() {
                jobs.push((ctx.clone(), header));
            }
        }
        let mut replies: HashMap<usize, String> = HashMap::new();
        let budget = self.seams.profile_lookup_budget_secs;
        if !jobs.is_empty() {
            let (tx, rx) = std_mpsc::channel::<(usize, anyhow::Result<String>)>();
            for (i, (_, header)) in jobs.iter().enumerate() {
                let (tx, header, fetch) = (tx.clone(), header.clone(), self.seams.fetch_accounts.clone());
                let spawned = std::thread::Builder::new().name("chrome-profiles".into()).spawn(move || {
                    let _ = tx.send((i, fetch(ACCOUNTS_URL, &header, budget)));
                });
                if spawned.is_err() {
                    continue;
                }
            }
            drop(tx);
            // The budget runs from the requests' submission, as concurrent.futures.wait's timeout does; a
            // straggler finishes on its own and is dropped.
            let deadline = deadline_after(Instant::now(), budget);
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                match rx.recv_timeout(left) {
                    Ok((i, Ok(body))) => {
                        replies.insert(i, body);
                    }
                    Ok((_, Err(_))) => {}
                    Err(_) => break,
                }
            }
        }
        let mut found: IndexMap<String, Arc<dyn ContextHandle>> = IndexMap::new();
        for (i, (ctx, _)) in jobs.iter().enumerate() {
            let accounts = accounts_in(replies.get(&i).map(String::as_str).unwrap_or(""));
            if let Some(first) = accounts.first() {
                if !found.contains_key(first) {
                    found.insert(first.clone(), ctx.clone());
                }
            }
        }
        self.profile_map = Some(found.clone());
        let names: Vec<&str> = found.keys().map(String::as_str).collect();
        tracing::info!(
            "browser lane: Chrome profiles open: {} ({} contexts, {} with Google cookies, {} answered, {:.1} s)",
            if names.is_empty() { "none identified".to_string() } else { names.join(", ") },
            contexts.len(),
            jobs.len(),
            replies.len(),
            t0.elapsed().as_secs_f64()
        );
        Ok(found)
    }

    fn first_context(&self) -> anyhow::Result<Arc<dyn ContextHandle>> {
        let browser = self.browser.as_ref().ok_or_else(|| LaneError::err("AttributeError", "'NoneType' object has no attribute 'contexts'"))?;
        browser.contexts()?.into_iter().next().ok_or_else(index_error)
    }

    pub fn pick_profile(&mut self, wanted: &str) -> anyhow::Result<Arc<dyn ContextHandle>> {
        if self.config.own || self.config.steel {
            return self.first_context(); // eve's Chrome and Steel's have one profile: their own
        }
        let profiles = if wanted.is_empty() { IndexMap::new() } else { self.profiles()? };
        if !wanted.is_empty() && !profiles.contains_key(wanted) {
            let open = if profiles.is_empty() {
                String::new()
            } else {
                format!(" (open now: {})", profiles.keys().cloned().collect::<Vec<_>>().join(", "))
            };
            return Err(AttachError(format!("no open Chrome window for {wanted}: open one in that profile{open}")).into());
        }
        match lookup(&profiles, wanted) {
            Some(c) => Ok(c),
            None => self.first_context(),
        }
    }

    /// Point the lane at the profile a request names (or the configured default). Returns the account.
    pub fn use_profile(&mut self, request: &str) -> anyhow::Result<String> {
        self.ensure()?;
        let Some(browser) = self.browser.clone() else { return Ok(String::new()) };
        let contexts = browser.contexts()?;
        if contexts.len() <= 1 {
            return Ok(String::new()); // one profile: there is nothing to choose, nothing to ask
        }
        let profiles = self.profiles()?;
        let email = profile_for(request, &profiles, &self.config.chrome_profile);
        let target = match lookup(&profiles, &email) {
            Some(c) => c,
            None => contexts[0].clone(),
        };
        let same = self.context.as_ref().is_some_and(|c| same_context(c, &target));
        if !same {
            if let Some(p) = &self.page {
                let _ = p.page.close();
            }
            self.context = Some(target.clone());
            let page = target.new_page()?;
            page.bring_to_front()?;
            self.page = Some(self.make_page(page));
        }
        Ok(email)
    }

    /// `browser.is_connected()`, false when it cannot say.
    pub fn alive(&self) -> bool {
        self.browser.as_ref().is_some_and(|b| b.is_connected())
    }

    /// `connected`: the lane has a context.
    pub fn connected(&self) -> bool {
        self.context.is_some()
    }

    pub fn close(&mut self) {
        if self.browser.is_some() {
            // Attach mode: close eve's own tab and disconnect. Never the context or the browser — they are the
            // owner's Chrome, with every window and tab they have open.
            if let Some(p) = &self.page {
                let _ = p.page.close(); // the owner may have closed it already
            }
            if let Some(pw) = self.pw.as_mut() {
                let _ = pw.stop();
            }
            self.browser = None;
            self.context = None;
            self.pw = None;
            self.page = None;
            self.profile_map = None;
            if self.config.steel {
                (self.seams.steel_release)(); // Chrome closes and writes eve's sign-ins
            }
            return;
        }
        if let Some(c) = &self.context {
            let _ = c.close();
        }
        if let Some(pw) = self.pw.as_mut() {
            let _ = pw.stop();
        }
        self.context = None;
        self.pw = None;
        self.page = None;
    }

    // -- what the agent calls --

    pub fn outline(&mut self) -> anyhow::Result<Outline> {
        self.ensure()?;
        let p = self.page.as_ref().expect("ensured");
        let cleared = p.clear_consent(); // the planner reads the page, not the notice over it
        if !cleared.is_empty() {
            tracing::info!("browser lane: {}", cleared);
        }
        let snap = p.snapshot()?;
        let blocked = p.blocked();
        let here = p.current_url();
        if !blocked.is_empty() {
            let site = observed_site(&here);
            tracing::info!("browser lane: a bot check is up at {}", if site.is_empty() { "-" } else { &site });
        }
        Ok(Outline {
            app: "browser".to_string(),
            lines: outline_lines(&snap, OUTLINE_LIMIT),
            title: snap.title.clone(),
            url: navigation_destination(&here),
            site: observed_site(&here),
            vault: self.vault.as_ref().map(|v| v.lines()).unwrap_or_default(),
            blocked,
        })
    }

    /// A plan result for a bot-check page: status "blocked", and the sentence is what eve tells the owner.
    fn blocked_result(p: &Page, index: usize) -> Value {
        let site = observed_site(&p.current_url());
        tracing::info!("browser lane: plan blocked by a bot check at {}", if site.is_empty() { "-" } else { &site });
        json!({"status": "blocked", "next_index": index, "reason": "browser_challenge", "confirm": "",
               "sentence": p.blocked(), "ms": 0.0, "ledger": []})
    }

    pub fn run_plan(
        &mut self,
        plan_dict: &Value,
        goal: &str,
        start: usize,
        approved: &IndexMap<String, String>,
        success_rejected: bool,
    ) -> anyhow::Result<Value> {
        self.ensure()?;
        let source = str_or_empty(plan_dict.get("source"));
        let source = if source.is_empty() { "astra".to_string() } else { source };
        let plan = eve_plan_contract::parse_plan_with(
            plan_dict,
            goal,
            eve_plan_contract::ParseOptions { source: &source, ..Default::default() },
        )?;
        let mut ok: HashMap<i64, String> = HashMap::new();
        for (k, v) in approved {
            let i = eve_vault::pystr::int_of_str(k).ok_or_else(|| {
                LaneError::err("ValueError", format!("invalid literal for int() with base 10: {}", repr(k)))
            })?;
            ok.insert(i.clamp(i64::MIN as i128, i64::MAX as i128) as i64, v.clone());
        }
        let p = self.page.as_ref().expect("ensured");
        p.set_approve(None); // the executor grants each approved click its label (grant)

        let open_app = |name: &str| -> anyhow::Result<String> {
            if ["browser", "chrome", "chromium", "google chrome", "safari"].contains(&casefold(name).as_str()) {
                return Ok(format!("opened {name}")); // the browser is already the whole world here
            }
            Err(LaneError::err("ValueError", format!("{name} is not the browser: this plan belongs on the Mac lane")))
        };
        if let Some(first) = plan.steps.get(start) {
            if first.kind != "open_url" {
                p.snapshot()?; // the page the plan starts on: a bot check stops it before step 1
                if !p.blocked().is_empty() {
                    return Ok(Self::blocked_result(p, start));
                }
            }
        }
        let plain = &self.plain_http;
        let open_url = |url: &str| p.open_url(plain.get(url).map(String::as_str).unwrap_or(url));
        let asker: Option<&dyn StepAsk> = self.step_asker.as_deref().map(|a| a as &dyn StepAsk);
        let result = run_plan(
            &plan,
            p,
            p,
            asker,
            &open_app,
            &open_url,
            &|| "browser".to_string(),
            PlanOptions {
                start,
                approved: ok,
                decide: if asker.is_some() { "jev" } else { "keyword" }.to_string(),
                planned_for: "browser".to_string(),
                success_rejected,
                ..PlanOptions::default()
            },
        );
        if !p.blocked().is_empty() && result.status != "complete" {
            // A step landed on a bot check (the effectors refused everything after it): not a step that failed, a
            // page nobody here may pass. Blocked, with the owner's line, never handed on to be solved.
            let mut out = Self::blocked_result(p, result.next_index);
            out["ms"] = json!(py_round_n(result.ms, 1));
            out["ledger"] = Value::Array(result.ledger.iter().map(|e| e.to_dict()).collect());
            return Ok(out);
        }
        Ok(result.to_dict())
    }

    /// Read once the text stops growing: a web app (Gmail) says "Loading…" for seconds after the load event (live,
    /// 2026-09-23: 190 characters read too early). Polls every READ_POLL_SECS, up to READ_WAIT_SECS.
    pub fn page_text(&mut self, limit: usize) -> anyhow::Result<PageText> {
        self.ensure()?;
        let p = self.page.as_ref().expect("ensured");
        // a page with no body text reads as empty, never as a crash
        let body = || py_collapse(&p.page.inner_text("body", 5000).unwrap_or_default());
        let len = |s: &str| s.chars().count();
        let challenge = |text: &str| -> anyhow::Result<String> {
            let url = p.page.url();
            Ok(if browser_blocker(&url, &p.page.title()?, text).is_empty() { String::new() } else { blocked_line(&url) })
        };
        let cap = |s: &str| s.chars().take(limit).collect::<String>();
        // A Cloudflare check that clears by itself is waited out first (settle_check); a hard one is not.
        let mut text = p.settle_check(|| Ok(body()), |t: &String| Ok((p.page.url(), p.page.title()?, t.clone())))?;
        let deadline = p.now() + READ_WAIT_SECS;
        let blocked = challenge(&text)?;
        if !blocked.is_empty() {
            // a bot check never grows into the answer: no wait
            return Ok(PageText { title: p.page.title()?, url: p.page.url(), text: cap(&text), signed_out: false, blocked });
        }
        let rounds = (READ_WAIT_SECS / READ_POLL_SECS.max(0.05)) as usize + 1; // bounded if the clock stands still
        for _ in 0..rounds {
            if p.now() >= deadline {
                break;
            }
            p.sleep(READ_POLL_SECS);
            let again = body();
            if len(&again) <= len(&text) && len(&again) >= READ_ENOUGH_CHARS {
                break; // stopped growing, and there is something to read
            }
            if len(&again) >= len(&text) {
                text = again;
            }
        }
        let url = p.page.url();
        Ok(PageText {
            title: p.page.title()?,
            url: url.clone(),
            text: cap(&text),
            signed_out: SIGN_IN.is_match(&url),
            blocked: challenge(&text)?,
        })
    }

    pub fn open_url(&mut self, url: &str) -> anyhow::Result<String> {
        self.ensure()?.open_url(url)
    }

    /// The page itself (the browser viewport, full size), not the desktop. None when not connected: a picture
    /// never opens a new connection (which would ask the owner for Chrome access).
    pub fn screenshot(&mut self) -> Option<Vec<u8>> {
        self.context.as_ref()?;
        let page = self.ensure().ok()?;
        page.page.screenshot_jpeg(85, Some((SECRET_FIELDS, MASK_COLOR))).ok()
    }
}

/// One job on the lane's thread.
type Job = Box<dyn FnOnce(&mut LaneCore) + Send>;

/// Owns the browser and its thread. Every public coroutine hands one job to that thread.
pub struct BrowserLane {
    config: BrowserLaneConfig,
    jobs: parking_lot::Mutex<Option<std_mpsc::Sender<Job>>>,
    connected: Arc<AtomicBool>,
    /// chrome_lane: a plan executing here is progress
    pub plans_started: AtomicU64,
}

impl BrowserLane {
    /// `BrowserLane(config, step_asker=, launcher=, vault=)`.
    pub fn new(config: BrowserLaneConfig, options: LaneOptions) -> anyhow::Result<BrowserLane> {
        BrowserLane::from_core(LaneCore::new(config, options)?)
    }

    /// A lane over a prepared core (its seams, plain_http and page options set).
    pub fn from_core(core: LaneCore) -> anyhow::Result<BrowserLane> {
        let config = core.config.clone();
        let connected = Arc::new(AtomicBool::new(core.connected()));
        let flag = connected.clone();
        let (tx, rx) = std_mpsc::channel::<Job>();
        std::thread::Builder::new().name("browser-lane".into()).spawn(move || {
            let mut core = core;
            for job in rx {
                job(&mut core);
                flag.store(core.connected(), Ordering::SeqCst);
            }
        })?;
        Ok(BrowserLane { config, jobs: parking_lot::Mutex::new(Some(tx)), connected, plans_started: AtomicU64::new(0) })
    }

    pub fn config(&self) -> &BrowserLaneConfig {
        &self.config
    }

    /// Run `f` on the lane's thread and await its answer.
    pub async fn run<R, F>(&self, f: F) -> anyhow::Result<R>
    where
        R: Send + 'static,
        F: FnOnce(&mut LaneCore) -> R + Send + 'static,
    {
        let (tx, rx) = tokio::sync::oneshot::channel::<std::thread::Result<R>>();
        let job: Job = Box::new(move |core: &mut LaneCore| {
            let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(core)));
            let _ = tx.send(out);
        });
        let sent = match self.jobs.lock().as_ref() {
            Some(jobs) => jobs.send(job).is_ok(),
            None => false,
        };
        if !sent {
            return Err(LaneError::err("RuntimeError", "cannot schedule new futures after shutdown"));
        }
        match rx.await {
            Ok(Ok(r)) => Ok(r),
            Ok(Err(_)) => Err(LaneError::err("RuntimeError", "a browser lane job panicked")),
            Err(_) => Err(LaneError::err("RuntimeError", "the browser lane's thread is gone")),
        }
    }

    /// The lane has a context (a connection, or a launched browser).
    pub fn connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    /// Connect now, if not already.
    pub async fn connect(&self) -> anyhow::Result<()> {
        self.connect_inner(None).await
    }

    /// Connect now, if not already. In attach mode Chrome asks "Allow remote debugging?" for this connection;
    /// `answer_prompt` (chrome_consent's ConsentBroker.answer_own_connection) runs alongside, so the dialog raised
    /// by THIS connection is answered with the owner's own yes or no. It is dropped (cancelled) once the
    /// connection is up or has failed.
    pub async fn connect_answering<F, Fut>(&self, answer_prompt: F) -> anyhow::Result<()>
    where
        F: FnOnce() -> Fut,
        Fut: Future + Send + 'static,
    {
        if self.config.own || self.config.steel {
            // eve's own Chrome and Steel never ask: there is nothing to press
            return self.connect_inner(None).await;
        }
        let fut = answer_prompt();
        self.connect_inner(Some(Box::pin(async move {
            fut.await;
        })))
        .await
    }

    async fn connect_inner(&self, answering: Option<Pin<Box<dyn Future<Output = ()> + Send>>>) -> anyhow::Result<()> {
        let (has_context, alive) = self.run(|c| (c.context.is_some(), c.browser.is_none() || c.alive())).await?;
        if has_context {
            if alive {
                return Ok(());
            }
            tracing::info!("browser lane: the Chrome connection is gone (Chrome restarted?); reconnecting");
            self.run(|c| c.close()).await?; // then a fresh connection, and a fresh Allow
        }
        let connecting = self.run(|c| c.ensure().map(|_| ()));
        let Some(mut answering) = answering else {
            return connecting.await?;
        };
        tokio::pin!(connecting);
        let mut answered = false;
        loop {
            // the answer starts at once, beside the connection (Python's ensure_future)
            tokio::select! {
                biased;
                _ = &mut answering, if !answered => answered = true,
                r = &mut connecting => return r?,
            }
        }
    }

    pub async fn use_profile(&self, request: &str) -> anyhow::Result<String> {
        let request = request.to_string();
        self.run(move |c| c.use_profile(&request)).await?
    }

    /// Close (attach: eve's tab only) and stop the lane's thread.
    pub async fn close(&self) -> anyhow::Result<()> {
        let r = self.run(|c| c.close()).await;
        self.jobs.lock().take();
        r
    }

    pub async fn outline(&self) -> anyhow::Result<Outline> {
        self.run(|c| c.outline()).await?
    }

    pub async fn run_plan(
        &self,
        plan: Value,
        goal: &str,
        start: usize,
        approved: IndexMap<String, String>,
        success_rejected: bool,
    ) -> anyhow::Result<Value> {
        self.plans_started.fetch_add(1, Ordering::SeqCst);
        let goal = goal.to_string();
        self.run(move |c| c.run_plan(&plan, &goal, start, &approved, success_rejected)).await?
    }

    /// The page's title, URL and visible text (whitespace-collapsed, capped): what a question is answered from.
    pub async fn page_text(&self, limit: usize) -> anyhow::Result<PageText> {
        self.run(move |c| c.page_text(limit)).await?
    }

    pub async fn open_url(&self, url: &str) -> anyhow::Result<String> {
        let url = url.to_string();
        self.run(move |c| c.open_url(&url)).await?
    }

    pub async fn screenshot(&self) -> anyhow::Result<Option<Vec<u8>>> {
        self.run(|c| c.screenshot()).await
    }
}

/// `page_text`'s default limit.
pub const PAGE_TEXT_LIMIT: usize = 12000;

/// The same Jev step asker the desktop worker builds, or None (keyword gate alone). `environ` None reads the
/// process environment.
pub fn make_step_asker(environ: Option<&HashMap<String, String>>) -> Option<Box<dyn StepAsk + Send + Sync>> {
    if !env_on(environ, "EVE_JEV_STEP") {
        return None;
    }
    let route = match environ {
        Some(env) => eve_jev::route_config(env, ""),
        None => eve_jev::route_config(&eve_jev::ProcessEnv, ""),
    };
    let (url, key, model) = match route {
        Ok(r) => r,
        Err(e) => {
            tracing::info!("browser lane: jev is off ({}); the keyword gate decides alone", e);
            return None;
        }
    };
    let seconds = match environ {
        Some(env) => eve_jev::timeout_from_env(env),
        None => eve_jev::timeout_from_env(&eve_jev::ProcessEnv),
    };
    let predict = eve_jev::make_predict(
        &url,
        &key,
        &model,
        eve_jev::PredictOptions { timeout_s: seconds, ..eve_jev::PredictOptions::default() },
    );
    let call = move |state: &Value, questions: &Value| -> anyhow::Result<Value> {
        predict
            .predict(state, questions)
            .map_err(|f| anyhow::Error::new(eve_typed_ask::PredictError::new(f.kind(), f.to_string())))
    };
    Some(Box::new(eve_typed_ask::make_step_asker(call, eve_desktop_lanes::perf_counter, environ)))
}
