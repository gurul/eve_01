//! Updates' Composio sources: what needs the owner in the apps connected through Composio (docs/updates.md,
//! Sources). A port of `updates_composio.py`.
//!
//! Owner, 2026-10-16: "make sure updates is also linked to all the composio stuff". So the Updates pane also reads,
//! one source each: Slack (direct messages and mentions of the owner in the last 3 days that the owner has not
//! answered), Notion (pages edited in the last 14 days that hold an open to-do, mention the owner, or are assigned to
//! the owner and still open), Linear (open issues assigned to the owner, due within 14 days or updated in the last
//! 7), and the owner's OTHER Gmail and Google Calendar accounts (composio_tools.OTHER_ACCOUNT_TOOLKITS: the work inbox
//! of the last 7 days without promotions and social, and the work calendar of the next 7 days).
//!
//! How a read goes:
//!
//! * `Reader` is one gather's view of Composio: the owner's connected accounts are listed once, lazily
//!   (`ComposioRead::connected_accounts`), and every source reads through `Reader::call`.
//! * `Reader::call` runs only a slug in `READ_SLUGS`, an allow-list of exact slugs that only read, and checks each one
//!   again with composio_tools' own `consequential_slugs` and `is_read_only` before it reaches `read_tool` (which
//!   refuses a writing slug a third time). Nothing here can send, change or delete.
//! * A source is **off** when its toolkit has no account the owner connected (a sign-in started and not finished
//!   counts as none). It **failed** when Composio could not be read, or when its only connection has expired or
//!   failed: an expired connection is a lost source, not an empty one, so `Updates::rebuild` keeps its old cards and
//!   tries again.
//! * Each item is `updates::item`, clipped like the others, with a stable id (`REF_PREFIX`) and, when the app gave
//!   one, its own `link` (`safe_link`: https on that app's host only). The link is never shown to the model.
//!
//! Logs carry counts, kinds and exception types only, never a title, a sender, an id or an alias.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use futures::future::BoxFuture;
use regex::Regex;
use serde_json::Value;

use eve_transcripts::Dt;

use crate::seams::{ComposioRead, Obj};
use crate::updates::{Item, ReadError};
use crate::{Env, Mono};

pub const ENV_SWITCH: &str = "EVE_UPDATES_COMPOSIO";

/// The source kinds, each one app.
pub const KINDS: [&str; 5] = ["slack", "notion", "linear", "work_email", "work_calendar"];
/// The ref prefix of each kind (`updates::REF_KINDS`).
pub const REF_PREFIX: [(&str, &str); 5] =
    [("slack", "slack"), ("notion", "notion"), ("linear", "linear"), ("work_email", "wgmail"), ("work_calendar", "wcal")];
/// The Composio toolkit each kind reads.
pub const TOOLKIT: [(&str, &str); 5] = [
    ("slack", "slack"),
    ("notion", "notion"),
    ("linear", "linear"),
    ("work_email", "gmail"),
    ("work_calendar", "googlecalendar"),
];
/// The time limit of each source (updates::gather), in seconds. Notion reads a page's blocks one call at a time.
pub const LIMITS: [(&str, f64); 5] =
    [("slack", 30.0), ("notion", 45.0), ("linear", 25.0), ("work_email", 45.0), ("work_calendar", 30.0)];

/// Every slug this module may run. Each one only reads (tests check them against composio_tools.consequential_slugs).
pub const READ_SLUGS: [&str; 7] = [
    "SLACK_SEARCH_MESSAGES",
    "NOTION_GET_ABOUT_ME",
    "NOTION_SEARCH_NOTION_PAGE",
    "NOTION_FETCH_BLOCK_CONTENTS",
    "LINEAR_LIST_LINEAR_ISSUES",
    "GMAIL_FETCH_EMAILS",
    "GOOGLECALENDAR_EVENTS_LIST",
];

/// The hosts an item's link may point at, by kind (the app's own pages; a subdomain counts).
pub const LINK_HOSTS: [(&str, &[&str]); 5] = [
    ("slack", &["slack.com"]),
    ("notion", &["notion.so", "notion.com", "notion.site"]),
    ("linear", &["linear.app"]),
    ("work_email", &["google.com"]),
    ("work_calendar", &["google.com"]),
];

pub const SLACK_DAYS: i64 = 3;
/// per search; the owner's own messages: 100
pub const SLACK_COUNT: i64 = 50;
/// conversations
pub const SLACK_MAX: usize = 15;
pub const NOTION_DAYS: i64 = 14;
/// pages read from the search, the most recently edited first
pub const NOTION_SEARCH: i64 = 50;
/// pages whose blocks are read
pub const NOTION_PAGES: usize = 10;
pub const NOTION_MAX: usize = 15;
pub const NOTION_BLOCKS: i64 = 100;
pub const LINEAR_FIRST: i64 = 100;
pub const LINEAR_DUE_DAYS: i64 = 14;
pub const LINEAR_UPDATED_DAYS: i64 = 7;
pub const LINEAR_MAX: usize = 15;
pub const WORK_MAIL_MAX: usize = 30;
pub const WORK_CALENDAR_DAYS: i64 = 7;
/// accounts read per kind
pub const MAX_ACCOUNTS: usize = 3;

pub const ACTIVE: &str = "ACTIVE";
/// a connection that was made and no longer works
pub const LOST_STATES: [&str; 3] = ["EXPIRED", "FAILED", "INACTIVE"];
pub const DONE_WORDS: [&str; 13] = [
    "done",
    "complete",
    "completed",
    "closed",
    "canceled",
    "cancelled",
    "duplicate",
    "shipped",
    "resolved",
    "archived",
    "won't do",
    "wont do",
    "not doing",
];
pub const PRIORITY: [(i64, &str); 4] = [(1, "Urgent"), (2, "High"), (3, "Medium"), (4, "Low")];

pub static SLACK_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Z0-9]{3,32}\n?\z").expect("SLACK_ID"));
pub static SLACK_TS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d{9,12}\.\d{1,8}\n?\z").expect("SLACK_TS"));
pub static SAFE_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_.:-]{1,128}\n?\z").expect("SAFE_ID"));
pub static ALIAS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^a-z0-9_-]+").expect("ALIAS"));

/// The toolkit has no connected account: the source is off (`ReadError::Off`). Python's `SourceOff`.
pub fn source_off(message: impl Into<String>) -> ReadError {
    ReadError::off(message)
}

/// Composio could not be read for this source, or its connection has expired: the source failed (and is lost).
/// Python's `ReadFailed`.
pub fn read_failed(message: impl Into<String>) -> ReadError {
    ReadError::failed("ReadFailed", message)
}

/// A slug off the allow-list, or one composio_tools says writes. Python's `PermissionError`.
pub fn permission(message: impl Into<String>) -> ReadError {
    ReadError::failed("PermissionError", message)
}

/// On unless EVE_UPDATES_COMPOSIO is 0/false/no/off.
pub fn enabled(environ: Option<&Env>) -> bool {
    let _ = environ;
    todo!()
}

/// A bridge these sources can read through: full eve's ComposioBridge once its client is made.
pub fn usable(bridge: Option<&Arc<dyn ComposioRead>>) -> bool {
    let _ = bridge;
    todo!()
}

/// `url` when it is https on one of `kind`'s hosts (LINK_HOSTS; any kind's for None), with no user part, in
/// printable ASCII; else None.
pub fn safe_link(url: Option<&Value>, kind: Option<&str>) -> Option<String> {
    let _ = (url, kind);
    todo!()
}

/// `row` with its `link` when `url` passes `safe_link` for `kind`. Python's `_linked`.
pub fn linked(row: Item, url: Option<&Value>, kind: &str) -> Item {
    let _ = (row, url, kind);
    todo!()
}

/// An account's alias as it goes in a ref (`wgmail:<alias>/<thread>`): lowercase letters, digits, - and _.
pub fn alias_word(alias: &str) -> String {
    let _ = alias;
    todo!()
}

// ---- the reader ------------------------------------------------------------------------------------------------

/// One gather's reads through `bridge`. The connected accounts are listed once, by the first source that asks; the
/// others wait for that answer.
pub struct Reader {
    pub bridge: Arc<dyn ComposioRead>,
    /// the listing: None until asked; then the accounts, or the failing call's exception type name
    accounts: tokio::sync::Mutex<Option<Result<Vec<Obj>, String>>>,
    /// the slugs run, in order (tests)
    pub calls: parking_lot::Mutex<Vec<String>>,
}

impl Reader {
    pub fn new(bridge: Arc<dyn ComposioRead>) -> Self {
        Reader { bridge, accounts: tokio::sync::Mutex::new(None), calls: parking_lot::Mutex::new(Vec::new()) }
    }

    /// The connected accounts (mappings only), listed once. A failed listing fails every source that asks:
    /// "the connected accounts could not be listed (<type>)".
    pub async fn accounts(&self) -> Result<Vec<Obj>, ReadError> {
        let _ = &self.accounts;
        todo!()
    }

    /// `[(alias, id)]` of the accounts `kind` reads, at most MAX_ACCOUNTS. For the work kinds only accounts aliased
    /// other than the owner's own (composio_tools.PRIMARY_ALIAS). Off when there is none, failed when the only ones
    /// have expired.
    pub async fn accounts_for(&self, kind: &str) -> Result<Vec<(String, String)>, ReadError> {
        let _ = kind;
        todo!()
    }

    /// Run one allowed read → its `data`. A permission error for a slug off the allow-list or one composio_tools says
    /// writes; failed when the call failed or sent no data.
    pub async fn call(&self, slug: &str, arguments: &Obj, account_id: &str) -> Result<Obj, ReadError> {
        let _ = (slug, arguments, account_id);
        todo!()
    }
}

// ---- small readers ---------------------------------------------------------------------------------------------

/// A timestamp as ISO 8601: Slack's `ts` (epoch seconds, a number or a digit string), or an ISO string as it is (at
/// most 40 characters). Python's `_iso`.
pub fn iso(value: &Value) -> String {
    let _ = value;
    todo!()
}

/// An ISO time, aware (a naive one read as UTC), or None. Python's `_parse`.
pub fn parse(value: &Value) -> Option<Dt> {
    let _ = value;
    todo!()
}

/// The first ten characters when they are a YYYY-MM-DD date. Python's `_day`.
pub fn day(value: &Value) -> Option<String> {
    let _ = value;
    todo!()
}

// ---- Slack -----------------------------------------------------------------------------------------------------

pub static SLACK_USER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<@([A-Z0-9]+)(?:\|[^>]*)?>").expect("SLACK_USER"));
pub static SLACK_CHANNEL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<#[A-Z0-9]+\|([^>]*)>").expect("SLACK_CHANNEL"));
pub static SLACK_LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<((?:https?|mailto):[^>|]+)(?:\|([^>]*))?>").expect("SLACK_LINK"));
pub static SLACK_SPECIAL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<!(here|channel|everyone)[^>]*>").expect("SLACK_SPECIAL"));

/// Slack's markup as plain words: the owner's mention as "@you", another's as "@someone", a channel by its name, a
/// link by its label (or "(link)").
pub fn slack_text(text: &Value, owner: Option<&str>) -> String {
    let _ = (text, owner);
    todo!()
}

/// A search answer's `messages.matches`, mappings only. Python's `_matches`.
pub fn matches(data: &Obj) -> Vec<Obj> {
    let _ = data;
    todo!()
}

/// A match with a well-formed channel id and ts. Python's `_slack_ok`.
pub fn slack_ok(m: &Obj) -> bool {
    let _ = m;
    todo!()
}

/// Direct messages and mentions of the owner from the last SLACK_DAYS days that the owner has not answered since,
/// one item per conversation, newest first: `slack:<channel>/<ts of its newest message>`.
pub async fn slack_items(reader: &Reader, now: Dt) -> Result<Vec<Item>, ReadError> {
    let _ = (reader, now);
    todo!()
}

// ---- Notion ----------------------------------------------------------------------------------------------------

/// A rich-text list's plain text. Python's `_plain`.
pub fn plain(rich: Option<&Value>) -> String {
    let _ = rich;
    todo!()
}

/// A rich-text list mentions the user `owner`. Python's `_mentions`.
pub fn mentions(rich: Option<&Value>, owner: Option<&str>) -> bool {
    let _ = (rich, owner);
    todo!()
}

/// What a page's properties say: `{"title", "status", "due", "assigned", "done"}`.
pub fn notion_page(page: &Obj, owner: Option<&str>) -> Obj {
    let _ = (page, owner);
    todo!()
}

/// (open to-dos, lines that mention the owner) among a page's first-level blocks.
pub fn notion_blocks(data: &Obj, owner: Option<&str>) -> (Vec<String>, Vec<String>) {
    let _ = (data, owner);
    todo!()
}

/// Pages edited in the last NOTION_DAYS days that hold an open to-do, mention the owner, or are assigned to the
/// owner and not done; most recently edited first: `notion:<page>`. The blocks of at most NOTION_PAGES pages are
/// read, and none after `deadline` on `clock`.
pub async fn notion_items(
    reader: &Reader,
    now: Dt,
    deadline: Option<f64>,
    clock: &Mono,
) -> Result<Vec<Item>, ReadError> {
    let _ = (reader, now, deadline, clock);
    todo!()
}

// ---- Linear ----------------------------------------------------------------------------------------------------

/// Open issues assigned to the owner that are due within LINEAR_DUE_DAYS days (or overdue) or were updated in the
/// last LINEAR_UPDATED_DAYS days; the nearest due date first, then the latest update: `linear:<issue id>`.
pub async fn linear_items(reader: &Reader, now: Dt) -> Result<Vec<Item>, ReadError> {
    let _ = (reader, now);
    todo!()
}

// ---- the work Gmail and Calendar -------------------------------------------------------------------------------

/// A message's `labelIds` as strings. Python's `_labels`.
pub fn labels(m: &Obj) -> Vec<String> {
    let _ = m;
    todo!()
}

/// Each other Gmail account's inbox of the last 7 days without promotions and social (updates::MAIL_QUERY), one
/// item per thread with its newest message: `wgmail:<account>/<thread>`. A thread the owner wrote in after that
/// message says so.
pub async fn work_email_items(reader: &Reader, now: Dt) -> Result<Vec<Item>, ReadError> {
    let _ = (reader, now);
    todo!()
}

/// Each other calendar account's events of the next 7 days that the owner has not declined, soonest first, as
/// updates::calendar_items reads the owner's own: `wcal:<account>/<event>`.
pub async fn work_calendar_items(reader: &Reader, now: Dt) -> Result<Vec<Item>, ReadError> {
    let _ = (reader, now);
    todo!()
}

/// One source's read, as updates::gather runs it.
pub type ReadFn = Box<dyn FnOnce() -> BoxFuture<'static, Result<Vec<Item>, ReadError>> + Send>;

/// `(kind, read)` for each of KINDS, as updates::gather runs them; `read` is None (the source is off) without a
/// bridge these sources can read through. One Reader serves them all, so the accounts are listed once. Notion's
/// deadline is `mono() + limits["notion"] * 0.7`.
pub fn reads(
    bridge: Option<Arc<dyn ComposioRead>>,
    now: Dt,
    limits: &HashMap<String, f64>,
    mono: Mono,
) -> Vec<(&'static str, Option<ReadFn>)> {
    let _ = (bridge, now, limits, mono);
    todo!()
}
