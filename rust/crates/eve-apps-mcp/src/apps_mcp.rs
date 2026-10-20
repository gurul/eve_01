//! The owner's everyday apps, direct: Gmail, Google Calendar, Google Drive, Google Contacts, GitHub, Google Maps.
//!
//! Owner's decision, 2026-10-04: the apps eve uses every day go straight to the vendor, not through a broker
//! (Composio stays for Slack, Notion, Linear and the long tail, composio_tools.py). A third party in the middle
//! of the owner's mail, calendar and code is one more company holding their tokens, and one more layer to debug.
//!
//! * Gmail, Calendar, Drive, Contacts: Google's plain REST APIs (`eve_google_rest`), one Google OAuth sign-in for
//!   all. Google's Workspace MCP servers answer only for a Cloud project in the Workspace Developer Preview
//!   Program (live 2026-10-04: "Access to this tool requires that your Google Cloud project ... is enrolled"),
//!   which a personal gmail does not get, so they are not used.
//! * GitHub: `api.githubcopilot.com/mcp/`, with a fine-grained personal access token.
//! * Google Maps: `mapstools.googleapis.com/mcp` (Maps Grounding Lite: places, routes, weather), with an API key.
//!
//! How it fits:
//!
//! * The client here is a small Streamable HTTP MCP client: initialize, then tools/list and tools/call as
//!   JSON-RPC posts whose reply is JSON or an SSE stream.
//! * Each server's tools become Responses-API function tools named `<app>__<tool>` (`calendar__list_events`),
//!   so the brain calls them in one round. Their schemas are slimmed (output schemas and vendor extensions
//!   dropped, descriptions clipped): Calendar's raw list is ~130 KB, mostly output schemas the model never needs.
//! * Apps open on demand. A turn starts with only the apps its message names ("mail", "PR", "how long to")
//!   plus `open_app`, a small tool that loads more apps (several in one call) for the rest of the turn. All
//!   six apps' tools at once are ~30k tokens on every turn; most turns use none.
//! * Each app has a policy for calls that change something (`read`: refused and not even offered; `auto`
//!   or `write`: known private routine changes run; `ask`: every change needs the owner's yes/no).
//!   Consequential and unclassified changes always ask. A call reads only when the server marks the tool
//!   `readOnlyHint` and its name carries no writing verb, so a mislabelled tool still asks.
//! * `execute` never fails: errors come back as `{"ok": false, "reason": ...}`.
//!
//! Tokens never reach a log, a Debug or the model. Tool arguments may hold mail text, so logs carry tool names,
//! counts and error types only. `apps login google` (the CLI) does the one-time browser sign-in.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use base64::Engine as _;
use eve_google_rest::py::{
    decode_utf8, py_repr, py_str, rstrip, split_ws, squash_ws, strip, take_chars, truthy,
};
use eve_google_rest::{
    Body, GoogleRestClient, HttpRequest, HttpResponse, ReqwestTransport, Transport,
};
use fancy_regex::Regex as FancyRegex;
use futures::StreamExt;
use indexmap::IndexMap;
use parking_lot::RwLock;
use regex::Regex;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::github_repos;
use crate::py::{dumps, dumps_indent2, expanduser, form_value, id_matches, py_float, urlencode};

// ---- errors -----------------------------------------------------------------------------------------------

/// A failure as the Python raised it: `kind` is `type(e).__name__`, the Display is `str(e)`. The bridge never
/// lets one out of `execute`; it becomes `"<kind>: <message>"` for the model.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct CallError {
    pub kind: String,
    pub message: String,
}

impl CallError {
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            message: message.into(),
        }
    }

    /// `type(e).__name__`.
    pub fn type_name(&self) -> &str {
        &self.kind
    }
}

impl From<eve_google_rest::Error> for CallError {
    fn from(e: eve_google_rest::Error) -> Self {
        CallError::new(e.type_name(), e.to_string())
    }
}

impl From<eve_google_rest::TransportError> for CallError {
    fn from(e: eve_google_rest::TransportError) -> Self {
        CallError::new(e.kind.clone(), e.message)
    }
}

impl From<std::io::Error> for CallError {
    fn from(e: std::io::Error) -> Self {
        let kind = match e.kind() {
            std::io::ErrorKind::NotFound => "FileNotFoundError",
            std::io::ErrorKind::PermissionDenied => "PermissionError",
            _ => "OSError",
        };
        CallError::new(kind, e.to_string())
    }
}

/// `class McpError(Exception)`: the server refused, or answered with no reply.
pub const MCP_ERROR: &str = "McpError";
/// `class _SessionGone(Exception)`: the server forgot the session (404 while one is held).
const SESSION_GONE: &str = "_SessionGone";

/// `McpError(message)`.
pub fn mcp_error(message: impl Into<String>) -> CallError {
    CallError::new(MCP_ERROR, message)
}

// ---- constants --------------------------------------------------------------------------------------------

/// httpx logs each request line at INFO with its URL, and a Google API's URL carries the owner's words
/// (q=from:sam, a calendar search). Those lines are dropped for the apps' hosts; every other host still logs.
pub const APP_HOSTS: [&str; 8] = [
    "gmail.googleapis.com",
    "people.googleapis.com",
    "www.googleapis.com/calendar",
    "www.googleapis.com/drive",
    "www.googleapis.com/upload/drive",
    "mapstools.googleapis.com",
    "api.githubcopilot.com",
    "oauth2.googleapis.com",
];

/// `_quiet_app_urls`: whether a log line may be written (false for a line naming one of the apps' hosts).
pub fn quiet_app_urls(line: &str) -> bool {
    !APP_HOSTS.iter().any(|h| line.contains(h))
}

pub const CONFIG_DIR: &str = "~/.config/eve-bridge";
pub const PROTOCOL_VERSION: &str = "2025-06-18";
pub const DEFAULT_TIMEOUT_SECS: f64 = 60.0;
/// app and tool in a function name: calendar__list_events
pub const SEP: &str = "__";

const ON: [&str; 4] = ["1", "true", "yes", "on"];

/// One app: its function-name prefix (and policy key), how the owner calls it, its MCP url (none for Google's
/// plain APIs), which sign-in it uses (google | github | maps), its default policy for a call that changes
/// something (read | auto | write | ask), and any headers its server needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    pub app: &'static str,
    pub label: &'static str,
    pub url: &'static str,
    pub auth: &'static str,
    pub policy: &'static str,
    pub headers: &'static [(&'static str, &'static str)],
}

/// Routine private changes run; sending, deleting, sharing and other consequential changes ask.
/// Explicit read/ask overrides stay stricter. Contacts only look (owner, 2026-10-09).
/// EVE_APPS_POLICY="gmail=read,github=write" overrides or extends this.
pub static SERVERS: LazyLock<IndexMap<&'static str, Server>> = LazyLock::new(|| {
    [
        // Google's: their plain APIs (eve_google_rest), so no MCP url.
        Server {
            app: "gmail",
            label: "Gmail",
            url: "",
            auth: "google",
            policy: "auto",
            headers: &[],
        },
        Server {
            app: "calendar",
            label: "Google Calendar",
            url: "",
            auth: "google",
            policy: "auto",
            headers: &[],
        },
        Server {
            app: "drive",
            label: "Google Drive",
            url: "",
            auth: "google",
            policy: "auto",
            headers: &[],
        },
        Server {
            app: "contacts",
            label: "Google Contacts",
            url: "",
            auth: "google",
            policy: "read",
            headers: &[],
        },
        // Its toolsets limited to the owner's everyday ones (the full server is 100+ tools), plus single tools for
        // "did CI pass": X-MCP-Tools composes with X-MCP-Toolsets (github-mcp-server docs/remote-server.md); the
        // token needs Actions: read for them. Notifications are left out: GitHub serves them to a classic token
        // only (live 2026-10-04, "403 Resource not accessible by personal access token"). Read-only mode is not
        // used: it would hide writes that the local policy can run or ask about.
        Server {
            app: "github",
            label: "GitHub",
            url: "https://api.githubcopilot.com/mcp/",
            auth: "github",
            policy: "auto",
            headers: &[
                ("X-MCP-Toolsets", "context,repos,issues,pull_requests,users"),
                ("X-MCP-Tools", "actions_list,actions_get,get_job_logs"),
            ],
        },
        // Every Maps tool only looks; read keeps it that way if one ever does not.
        Server {
            app: "maps",
            label: "Google Maps",
            url: "https://mapstools.googleapis.com/mcp",
            auth: "maps",
            policy: "read",
            headers: &[],
        },
    ]
    .into_iter()
    .map(|s| (s.app, s))
    .collect()
});

// The words that open an app at the start of a turn, before the model asks. A miss costs a whole model round
// (open_app, then the call); a false hit costs only that app's tokens on that turn. So a compound request
// opens every app it needs: "my next class on the calendar and a maps link" is the calendar and Maps at once
// (owner, 2026-10-04: "it should do it fast because of all the integrations"). Bare "today" and "tomorrow"
// open nothing: they ride along with most requests. "finals" never matches "finally" (\b). A bare "build" is
// not GitHub's: "build me an app" is eve's app maker, and GitHub's schemas are the largest (~44 KB).
const APOS: &str = "['\u{2019}]";

pub static APP_WORDS: LazyLock<IndexMap<&'static str, FancyRegex>> = LazyLock::new(|| {
    let patterns: [(&str, String); 6] = [
        (
            "gmail",
            r"\b(e-?mails?|e-?mail(?:ed|ing)|mails?|inbox|gmail|unread|newsletters?|receipts?|sent me|(?:write|reply) (?:back )?to (?:him|her|them)|reply (?:to )?(?:that|this|the) (?:message|thread))\b".to_string(),
        ),
        // someone else's address or number: "Sam's email" alone is a message, and "my email address" is the
        // owner's own
        (
            "contacts",
            String::new()
                + r"\b(contacts?|phone numbers?|(?<!my )(?<!your )e-?mail address(?:es)?|(?:his|her|their) (?:number|"
                + r"e-?mail address)|\w+"
                + APOS
                + r"s (?:number|phone|cell|e-?mail address)|number for|"
                // "email sam that…": the address comes from the contacts
                + r"(?:(?:^|(?<=please )|(?<=pls )|(?<=can you )|(?<=could you )|(?<=and )|(?<=then )|(?<=also ))"
                + r"e-?mail|send (?:an? )?e-?mail to|write an e-?mail to) (?!(?:me|it|the|my|this|that|from|about|"
                + r"back|to|is|was|address|addresses|and|or|for|i|you)\b)[a-z]\w+)\b",
        ),
        (
            "calendar",
            String::new()
                + r"\b(calendars?|meetings?|events?|appointments?|agenda|(?:re)?schedule[ds]?|busy|"
                + r"(?:am i|are we|is \w+) free|free (?:at|on|this|next|tomorrow|today|tonight|later|before|after)|"
                + r"free ?/ ?busy|availability|invites?|invitations?|rsvp|book (?:a|an|the) (?:slot|time)|"
                + r"class(?:es)?|lectures?|labs?|sections?|seminars?|office hours|exams?|midterms?|finals?|"
                + r"quiz(?:zes)?|flights?|what"
                + APOS
                + r"?s next|whats next|up next|my day)\b",
        ),
        (
            "drive",
            r"\b(drive(?! (?:to|there|from|home|back|over))|google docs?|docs?|documents?|sheets?|spreadsheets?|slides|folders?|pdfs?)\b".to_string(),
        ),
        (
            "github",
            String::new()
                + r"\b(github|gh|repos?|repository|repositories|pull requests?|prs?|commits?|branch(?:es)?|"
                + r"ci|builds|(?:the|my|last|latest|ci) build|build (?:pass|passed|fail|failed|failing|broke|broken|"
                + r"status|is|was|green|red)|workflows?|actions? (?:run|runs|workflow|failed|failing|passed))\b",
        ),
        (
            "maps",
            String::new()
                + r"\b(maps?(?: link)?|directions?|navigate|navigation|routes?|how (?:long|far)|get(?:ting)? (?:there|to)|"
                + r"(?:drive|driving|walk|walking|bike|biking|cycle|cycling) (?:to|there|from)|commute|traffic|eta|"
                + r"nearby|near me|near (?:the|my|here)|closest|nearest|where"
                + APOS
                + r"?s|where (?:is|are)|"
                + r"(?<!e-mail )(?<!email )address(?:es)?|location|weather|forecast|rain(?:ing)?|"
                + r"restaurants?|caf(?:e|é)s?|(?:coffee|food|bars?|lunch|dinner|brunch) (?:near|nearby|around))\b",
        ),
    ];
    patterns
        .into_iter()
        .map(|(app, p)| {
            (
                app,
                FancyRegex::new(&format!("(?i){p}")).expect("APP_WORDS pattern"),
            )
        })
        .collect()
});

// An issue is GitHub's only when the message names no other home for it (review, 2026-10-05): "add a linear
// issue for the login bug", "the open issues in linear" and "any issues with my order on uber eats" opened
// GitHub, the largest schemas, on every round of a turn that never used it. Bare "actions" and "merge" ("what
// actions do I need to take", "merge these two docs") are GitHub's only beside a GitHub word above (PR, branch,
// CI, repo).
pub static GITHUB_ISSUE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(issues?)\b").expect("GITHUB_ISSUE"));
pub static ISSUE_ELSEWHERE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(linear|notion|slack|jira|asana|trello|uber ?eats|instacart|orders?|deliver(?:y|ies))\b")
        .expect("ISSUE_ELSEWHERE")
});

/// The owner's repository names that open GitHub as a word (github_repos::keyword_names), set when the apps
/// start.
static REPO_NAMES: LazyLock<RwLock<Arc<BTreeSet<String>>>> =
    LazyLock::new(|| RwLock::new(Arc::new(BTreeSet::new())));

/// `REPO_NAMES`, as it is now.
pub fn repo_names() -> Arc<BTreeSet<String>> {
    REPO_NAMES.read().clone()
}

/// Set `REPO_NAMES` (what `AppsBridge::start` does once the repositories are loaded).
pub fn set_repo_names(names: BTreeSet<String>) {
    *REPO_NAMES.write() = Arc::new(names);
}

/// Whether a message names this app (APP_WORDS, an issue with no other home, or one of the owner's repository
/// names, for GitHub).
pub fn names_app(app: &str, text: &str) -> bool {
    let folded = python_ignorecase(text);
    if let Some(rx) = APP_WORDS.get(app) {
        if rx.is_match(folded.as_ref()).unwrap_or(false) {
            return true;
        }
    }
    if app == "github" {
        let names = repo_names();
        if !names.is_empty() && github_repos::names_repo(text, &names) {
            return true;
        }
    }
    app == "github" && GITHUB_ISSUE.is_match(&folded) && !ISSUE_ELSEWHERE.is_match(&folded)
}

/// Python's `re.IGNORECASE` also lets the Turkish dotted and dotless I (U+0130, U+0131) match an ASCII `i`, which
/// Unicode simple case folding (the regex crate's) does not; read them as `i` before matching, so "İnbox" opens
/// Gmail as it does in Python. Every other non-ASCII character that matches an ASCII letter there (ſ, K) folds
/// the same way in both.
fn python_ignorecase(text: &str) -> std::borrow::Cow<'_, str> {
    if text.contains(['\u{130}', '\u{131}']) {
        std::borrow::Cow::Owned(text.replace(['\u{130}', '\u{131}'], "i"))
    } else {
        std::borrow::Cow::Borrowed(text)
    }
}

pub const OPEN_TOOL: &str = "open_app";

/// The auth kinds whose token is an API key in this header, not a Bearer token.
pub const KEY_HEADERS: [(&str, &str); 1] = [("maps", "X-Goog-Api-Key")];
pub const POLICIES: [&str; 4] = ["read", "auto", "write", "ask"];

fn key_header(auth: &str) -> &'static str {
    KEY_HEADERS
        .iter()
        .find(|(k, _)| *k == auth)
        .map(|(_, h)| *h)
        .unwrap_or("")
}

pub const READ_VERBS: [&str; 17] = [
    "get", "list", "fetch", "search", "find", "read", "retrieve", "lookup", "check", "count",
    "describe", "query", "view", "download", "show", "preview", "suggest",
];
pub const WRITE_VERBS: [&str; 73] = [
    "send", "create", "delete", "remove", "update", "edit", "modify", "post", "publish", "merge",
    "transfer", "submit", "cancel", "archive", "trash", "untrash", "reply", "forward", "invite",
    "share", "add", "set", "move", "upload", "insert", "write", "patch", "put", "execute", "run",
    "trigger", "mark", "unmark", "label", "unlabel", "assign", "close", "revoke", "grant",
    "rename", "copy", "import", "sync", "reset", "clear", "purge", "draft", "compose", "schedule",
    "accept", "decline", "approve", "reject", "respond", "react", "apply", "fork", "push",
    "dismiss", "lock", "unlock", "star", "unstar", "pin", "unpin", "pay", "buy", "purchase",
    "charge", "withdraw", "book", "order", "deploy",
];
/// Words that are also plain nouns: a write only in verb position (the first word). get_draft reads.
pub const NOUN_TOO: [&str; 8] = [
    "label", "draft", "run", "schedule", "star", "pin", "lock", "order",
];

/// a tool's description
pub const DESC_CLIP: usize = 400;
/// each description inside its schema
pub const PROP_CLIP: usize = 160;
/// characters of one result handed back to the model
pub const RESULT_CLIP: usize = 60_000;
pub const VALUE_CLIP: usize = 60;
pub const LINE_CLIP: usize = 300;

/// open_app over `closed` ((name, label) of each toolset not yet open), or None with none closed. Its enum is
/// exactly the closed names, so the model can only name a toolset that is there to open. AppsBridge::open_tool
/// passes the direct apps; since 2026-10-05 telegram passes Share and the workspace too, when the toolset router
/// decides a turn's tools (toolset_router).
pub fn open_tool_spec(closed: &[(&str, &str)]) -> Option<Value> {
    let names: Vec<&str> = closed.iter().map(|(name, _)| *name).collect();
    if names.is_empty() {
        return None;
    }
    let listed = closed
        .iter()
        .map(|(name, label)| format!("{name} = {label}"))
        .collect::<Vec<_>>()
        .join("; ");
    Some(
        json!({"type": "function", "name": OPEN_TOOL, "strict": false,
        "description": format!("Load the tools of the owner's apps for the rest of this turn, then call them. Name \
                                every app the request needs in one call. Apps: {listed}."),
        "parameters": {"type": "object", "properties": {"apps": {
            "type": "array", "items": {"type": "string", "enum": names}, "minItems": 1}},
            "required": ["apps"]}}),
    )
}

/// The toolsets an open_app call names (`apps`, or the older single `app`), lower-cased, each once.
pub fn open_asked(args: &Map<String, Value>) -> Vec<String> {
    let mut asked: Vec<Value> = match args.get("apps") {
        Some(s @ Value::String(_)) => vec![s.clone()],
        Some(Value::Array(items)) => items.clone(),
        _ => Vec::new(),
    };
    if let Some(app) = args.get("app").filter(|a| truthy(a)) {
        asked.push(app.clone());
    }
    let mut out: Vec<String> = Vec::new();
    for a in asked {
        if !truthy(&a) || strip(&py_str(&a)).is_empty() {
            continue;
        }
        let name = strip(&py_str(&a)).to_lowercase();
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

// ---- configuration ----------------------------------------------------------------------------------------

/// The environment as a map (Python's `os.environ` or the mapping a caller passes).
pub type Environ = HashMap<String, String>;
/// Each app's policy for writing calls, in the order the Python dict holds them.
pub type Policy = IndexMap<String, String>;

/// `os.environ`, read now.
pub fn os_environ() -> Environ {
    std::env::vars().collect()
}

fn truthy_env(value: Option<&String>) -> bool {
    let v = value.map(|s| strip(s).to_lowercase()).unwrap_or_default();
    ON.contains(&v.as_str())
}

/// What the bridge reads from the environment: the switch, where the sign-ins are kept, the timeout, each
/// app's policy and the environment itself (the GitHub token and Maps key are read from it).
#[derive(Debug, Clone)]
pub struct AppsConfig {
    pub enabled: bool,
    pub config_dir: PathBuf,
    pub timeout_secs: f64,
    pub policy: Policy,
    pub environ: Environ,
}

impl Default for AppsConfig {
    fn default() -> Self {
        Self::new(false)
    }
}

impl AppsConfig {
    /// `AppsConfig(enabled)` with every other field at its default.
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            config_dir: PathBuf::from(CONFIG_DIR),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            policy: Policy::new(),
            environ: Environ::new(),
        }
    }

    pub fn google_client_path(&self) -> PathBuf {
        expanduser(&self.config_dir).join("google-oauth-client.json")
    }

    pub fn google_token_path(&self) -> PathBuf {
        expanduser(&self.config_dir).join("google-token.json")
    }
}

/// Each app's policy for writing calls: the defaults, then EVE_APPS_POLICY on top. `None` reads the process
/// environment.
pub fn policy(environ: Option<&Environ>) -> Policy {
    let owned;
    let env = match environ {
        Some(e) => e,
        None => {
            owned = os_environ();
            &owned
        }
    };
    let mut out: Policy = SERVERS
        .iter()
        .map(|(app, s)| (app.to_string(), s.policy.to_string()))
        .collect();
    let raw = env.get("EVE_APPS_POLICY").cloned().unwrap_or_default();
    for piece in raw.split(',') {
        let piece = strip(piece).to_lowercase();
        let Some((app, value)) = piece.split_once('=') else {
            continue;
        };
        if app.is_empty() {
            continue;
        }
        if !POLICIES.contains(&value) {
            tracing::warn!(
                "apps: EVE_APPS_POLICY has {} for {}; one of ('read', 'auto', 'write', 'ask')",
                eve_google_rest::py::repr_str(value),
                app
            );
            continue;
        }
        out.insert(app.to_string(), value.to_string());
    }
    out
}

/// On with EVE_APPS=1. Each app then comes up only if its sign-in is there (`TokenSource::available`). `None`
/// reads the process environment.
pub fn configured(environ: Option<&Environ>) -> AppsConfig {
    let env = environ.cloned().unwrap_or_else(os_environ);
    let raw = env.get("EVE_APPS_TIMEOUT_SECS").filter(|s| !s.is_empty());
    let timeout = match raw {
        Some(s) => crate::py::parse_float(s).unwrap_or(DEFAULT_TIMEOUT_SECS),
        None => DEFAULT_TIMEOUT_SECS,
    };
    let dir = env
        .get("EVE_APPS_DIR")
        .filter(|s| !s.is_empty())
        .cloned()
        .unwrap_or_else(|| CONFIG_DIR.to_string());
    AppsConfig {
        enabled: truthy_env(env.get("EVE_APPS")),
        config_dir: PathBuf::from(dir),
        // `timeout > 0` is false for NaN, as in Python
        timeout_secs: if timeout > 0.0 {
            timeout
        } else {
            DEFAULT_TIMEOUT_SECS
        },
        policy: policy(Some(&env)),
        environ: env,
    }
}

/// A timeout as reqwest can take it: an infinite one (`float("inf")`, httpx waits forever) is a century.
fn finite_timeout(secs: f64) -> f64 {
    if secs.is_finite() {
        secs
    } else {
        100.0 * 365.0 * 24.0 * 3600.0
    }
}

// ---- tokens -----------------------------------------------------------------------------------------------

/// `_read_json`: the file's JSON object, or `{}` when it is missing, unreadable, not JSON or not an object.
pub fn read_json(path: &Path) -> Map<String, Value> {
    let Ok(bytes) = std::fs::read(path) else {
        return Map::new();
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return Map::new();
    };
    match crate::py::json_loads(&text) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    }
}

/// Atomically, owner-only (0600): these files hold tokens. Written as `json.dump(data, f, indent=2)` plus a
/// newline.
pub fn write_private_json(path: &Path, data: &Map<String, Value>) -> std::io::Result<()> {
    use std::io::Write as _;
    #[cfg(unix)]
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".tmp");
    let tmp = path.with_file_name(name);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    opts.mode(0o600);
    let mut f = opts.open(&tmp)?;
    f.write_all(dumps_indent2(&Value::Object(data.clone())).as_bytes())?;
    f.write_all(b"\n")?;
    drop(f);
    #[cfg(unix)]
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    std::fs::rename(&tmp, path)
}

pub const GOOGLE_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

/// The Google scopes the policy needs, no wider: Gmail reads, and sends (gmail.send: sending only, no deleting
/// or labelling) unless it is read only; the calendar's own scope covers listing calendars and changing events;
/// Drive reads everything and writes only files eve made; contacts are read, saved and other.
pub fn google_scopes(pol: &Policy) -> Vec<String> {
    let g = "https://www.googleapis.com/auth/";
    let get = |app: &str, default: &'static str| {
        pol.get(app)
            .map(String::as_str)
            .unwrap_or(default)
            .to_string()
    };
    let mut scopes = vec![format!("{g}gmail.readonly")];
    if get("gmail", "ask") != "read" {
        scopes.push(format!("{g}gmail.send"));
    }
    scopes.push(if get("calendar", "write") != "read" {
        format!("{g}calendar")
    } else {
        format!("{g}calendar.readonly")
    });
    scopes.push(format!("{g}drive.readonly"));
    if get("drive", "ask") != "read" {
        scopes.push(format!("{g}drive.file"));
    }
    scopes.push(format!("{g}contacts.readonly"));
    scopes.push(format!("{g}contacts.other.readonly"));
    scopes
}

/// The kinds whose secret is a fixed value in the env file, read as it is.
pub const ENV_TOKENS: [(&str, &str); 2] = [
    ("github", "EVE_GITHUB_MCP_TOKEN"),
    ("maps", "EVE_GOOGLE_MAPS_KEY"),
];

fn env_token_name(kind: &str) -> Option<&'static str> {
    ENV_TOKENS.iter().find(|(k, _)| *k == kind).map(|(_, n)| *n)
}

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// One app's sign-in as the bridge holds it: the token (`eve_google_rest::TokenSource`), and whether it is there
/// at all. `available` is None for a source that cannot say (a plain closure: Python's
/// `getattr(src, "available", None)`), which counts as there.
pub trait AppToken: eve_google_rest::TokenSource {
    fn available(&self) -> Option<bool> {
        None
    }
}

/// A plain closure as a token source (a lambda in the Python's `tokens=`).
pub struct FnToken<F>(pub F);

#[async_trait]
impl<F> eve_google_rest::TokenSource for FnToken<F>
where
    F: Fn() -> Option<String> + Send + Sync,
{
    async fn token(&self) -> Option<String> {
        (self.0)()
    }
}

impl<F> AppToken for FnToken<F> where F: Fn() -> Option<String> + Send + Sync {}

/// A closure as a shared token source.
pub fn token_fn<F>(f: F) -> Arc<dyn AppToken>
where
    F: Fn() -> Option<String> + Send + Sync + 'static,
{
    Arc::new(FnToken(f))
}

/// A shared token source handed to a client that owns its token.
#[derive(Clone)]
pub struct SharedToken(pub Arc<dyn AppToken>);

#[async_trait]
impl eve_google_rest::TokenSource for SharedToken {
    async fn token(&self) -> Option<String> {
        self.0.token().await
    }
}

/// One app's bearer token, refreshed when it is about to expire. `get` returns the token or None (not signed
/// in); it never fails past a log line, so one app's broken sign-in never stops the others.
pub struct TokenSource {
    pub kind: String,
    pub config: AppsConfig,
    http: Option<Arc<dyn Transport>>,
    lock: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for TokenSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TokenSource({})", self.kind)
    }
}

impl TokenSource {
    pub fn new(kind: &str, config: AppsConfig) -> Self {
        Self {
            kind: kind.to_string(),
            config,
            http: None,
            lock: tokio::sync::Mutex::new(()),
        }
    }

    /// `TokenSource(kind, config, http=...)`: refreshes go out over this transport.
    pub fn with_transport(kind: &str, config: AppsConfig, http: Arc<dyn Transport>) -> Self {
        Self {
            http: Some(http),
            ..Self::new(kind, config)
        }
    }

    pub fn available(&self) -> bool {
        if let Some(name) = env_token_name(&self.kind) {
            return !strip(
                self.config
                    .environ
                    .get(name)
                    .map(String::as_str)
                    .unwrap_or(""),
            )
            .is_empty();
        }
        if self.kind == "google" {
            return read_json(&self.config.google_token_path())
                .get("refresh_token")
                .is_some_and(truthy);
        }
        false
    }

    /// `__call__`: the token, or None.
    pub async fn get(&self) -> Option<String> {
        if let Some(name) = env_token_name(&self.kind) {
            let v = strip(
                self.config
                    .environ
                    .get(name)
                    .map(String::as_str)
                    .unwrap_or(""),
            )
            .to_string();
            return if v.is_empty() { None } else { Some(v) };
        }
        let _held = self.lock.lock().await;
        if self.kind != "google" {
            return None;
        }
        match self.google().await {
            Ok(tok) => tok,
            Err(e) => {
                // the type only; a token is never logged
                tracing::warn!(
                    "apps: {} token refresh failed ({})",
                    self.kind,
                    e.type_name()
                );
                None
            }
        }
    }

    async fn post(
        &self,
        url: &str,
        data: &[(&str, String)],
    ) -> Result<Map<String, Value>, CallError> {
        let http: Arc<dyn Transport> = match &self.http {
            Some(h) => h.clone(),
            None => Arc::new(ReqwestTransport::new(30.0)),
        };
        let request = HttpRequest {
            method: "POST".into(),
            url: url.into(),
            headers: vec![(
                "Content-Type".into(),
                "application/x-www-form-urlencoded".into(),
            )],
            body: Some(urlencode(data).into_bytes()),
        };
        let r = http.send(request).await?;
        let (_, _, content) = read_response(r).await?;
        match parse_json(&content)? {
            Value::Object(m) => Ok(m),
            _ => Err(CallError::new(
                "ValueError",
                "token endpoint answered with no object",
            )),
        }
    }

    async fn google(&self) -> Result<Option<String>, CallError> {
        let path = self.config.google_token_path();
        let mut tok = read_json(&path);
        let Some(refresh) = tok.get("refresh_token").filter(|v| truthy(v)).cloned() else {
            return Ok(None);
        };
        if let Some(access) = tok.get("access_token").filter(|v| truthy(v)) {
            let expires = py_float(
                tok.get("expires_at")
                    .filter(|v| truthy(v))
                    .unwrap_or(&json!(0)),
            )?;
            if expires > now() + 60.0 {
                return Ok(Some(py_str(access)));
            }
        }
        let client = google_client(&self.config.google_client_path())?;
        let body = self
            .post(
                GOOGLE_TOKEN_URL,
                &[
                    ("grant_type", "refresh_token".to_string()),
                    ("refresh_token", form_value(&refresh)),
                    (
                        "client_id",
                        form_value(client.get("client_id").unwrap_or(&Value::Null)),
                    ),
                    (
                        "client_secret",
                        form_value(client.get("client_secret").unwrap_or(&json!(""))),
                    ),
                ],
            )
            .await?;
        let Some(access) = body.get("access_token").filter(|v| truthy(v)).cloned() else {
            let why = body
                .get("error")
                .filter(|v| truthy(v))
                .map(py_str)
                .unwrap_or_else(|| "no token".into());
            tracing::warn!(
                "apps: Google refused the refresh ({why}); sign in again: apps login google"
            );
            return Ok(None);
        };
        let expires_in = py_float(
            body.get("expires_in")
                .filter(|v| truthy(v))
                .unwrap_or(&json!(3600)),
        )?;
        tok.insert("access_token".into(), access.clone());
        tok.insert("expires_at".into(), json!(now() + expires_in));
        write_private_json(&path, &tok)?;
        Ok(Some(py_str(&access)))
    }
}

#[async_trait]
impl eve_google_rest::TokenSource for TokenSource {
    async fn token(&self) -> Option<String> {
        self.get().await
    }
}

impl AppToken for TokenSource {
    fn available(&self) -> Option<bool> {
        Some(TokenSource::available(self))
    }
}

/// `_google_client`: the Desktop OAuth client the owner downloaded from their own Cloud project.
pub fn google_client(path: &Path) -> Result<Map<String, Value>, CallError> {
    let data = read_json(path);
    let inner = match (
        data.get("installed").filter(|v| truthy(v)),
        data.get("web").filter(|v| truthy(v)),
    ) {
        (Some(v), _) | (None, Some(v)) => v.clone(),
        (None, None) => Value::Object(data.clone()),
    };
    match inner {
        Value::Object(m) if m.get("client_id").is_some_and(truthy) => Ok(m),
        _ => Err(CallError::new(
            "ValueError",
            format!("no OAuth client in {}", path.display()),
        )),
    }
}

// ---- the MCP client ---------------------------------------------------------------------------------------

/// The JSON-RPC messages in an SSE body: each event's data lines, joined.
pub fn sse_messages(text: &str) -> Vec<Map<String, Value>> {
    let mut out = Vec::new();
    let mut data: Vec<String> = Vec::new();
    let mut lines = eve_google_rest::py::splitlines(text);
    lines.push("");
    for line in lines {
        if let Some(rest) = line.strip_prefix("data:") {
            data.push(
                rest.trim_start_matches(eve_google_rest::py::is_space)
                    .to_string(),
            );
        } else if strip(line).is_empty() && !data.is_empty() {
            if let Ok(Value::Object(m)) = crate::py::json_loads(&data.join("\n")) {
                out.push(m);
            }
            data.clear();
        }
    }
    out
}

async fn read_response(
    r: HttpResponse,
) -> Result<(u16, Vec<(String, String)>, Vec<u8>), CallError> {
    let HttpResponse {
        status,
        headers,
        mut body,
    } = r;
    let mut content = Vec::new();
    while let Some(chunk) = body.next().await {
        content.extend_from_slice(&chunk?);
    }
    Ok((status, headers, content))
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// httpx's `Response.json()`: `json.loads` of the body's bytes, with Python's error class and text.
fn parse_json(content: &[u8]) -> Result<Value, CallError> {
    crate::py::json_loads_bytes(content)
}

/// Set a header as a dict assignment does: the same name replaced in place, else added at the end.
fn set_header(headers: &mut Vec<(String, String)>, name: &str, value: String) {
    match headers.iter_mut().find(|(k, _)| k == name) {
        Some(slot) => slot.1 = value,
        None => headers.push((name.to_string(), value)),
    }
}

struct McpState {
    session: Option<String>,
    /// what initialize agreed, sent on every later request
    version: String,
    ready: bool,
    ids: i64,
}

/// A Streamable HTTP MCP client. One logical session, reopened when the server forgets it.
pub struct McpClient {
    pub url: String,
    token: Arc<dyn eve_google_rest::TokenSource>,
    /// the token goes here, as it is, instead of Authorization
    key_header: String,
    headers: Vec<(String, String)>,
    http: Arc<dyn Transport>,
    state: tokio::sync::Mutex<McpState>,
}

impl McpClient {
    /// `McpClient(url, token, headers, timeout, key_header=...)` over reqwest.
    pub fn new(
        url: &str,
        token: impl eve_google_rest::TokenSource + 'static,
        headers: &[(&str, &str)],
        timeout_secs: f64,
        key_header: &str,
    ) -> Self {
        Self::with_transport(
            url,
            token,
            headers,
            Arc::new(ReqwestTransport::new(finite_timeout(timeout_secs))),
            key_header,
        )
    }

    /// `McpClient(..., http=...)`: the client over a given transport.
    pub fn with_transport(
        url: &str,
        token: impl eve_google_rest::TokenSource + 'static,
        headers: &[(&str, &str)],
        http: Arc<dyn Transport>,
        key_header: &str,
    ) -> Self {
        let mut own: Vec<(String, String)> = Vec::new();
        for (k, v) in headers {
            set_header(&mut own, k, v.to_string());
        }
        Self {
            url: url.to_string(),
            token: Arc::new(token),
            key_header: key_header.to_string(),
            headers: own,
            http,
            state: tokio::sync::Mutex::new(McpState {
                session: None,
                version: PROTOCOL_VERSION.to_string(),
                ready: false,
                ids: 0,
            }),
        }
    }

    async fn send(
        &self,
        st: &mut McpState,
        method: &str,
        params: Option<Map<String, Value>>,
        notify: bool,
    ) -> Result<Map<String, Value>, CallError> {
        let mut headers: Vec<(String, String)> = vec![
            ("Content-Type".into(), "application/json".into()),
            (
                "Accept".into(),
                "application/json, text/event-stream".into(),
            ),
            ("MCP-Protocol-Version".into(), st.version.clone()),
        ];
        for (k, v) in &self.headers {
            set_header(&mut headers, k, v.clone());
        }
        let token = self.token.token().await.filter(|t| !t.is_empty());
        if let Some(token) = token {
            if !self.key_header.is_empty() {
                set_header(&mut headers, &self.key_header, token);
            } else {
                set_header(&mut headers, "Authorization", format!("Bearer {token}"));
            }
        }
        if let Some(session) = st.session.clone().filter(|s| !s.is_empty()) {
            set_header(&mut headers, "Mcp-Session-Id", session);
        }
        let mut msg = Map::new();
        msg.insert("jsonrpc".into(), json!("2.0"));
        msg.insert("method".into(), json!(method));
        if let Some(p) = params {
            msg.insert("params".into(), Value::Object(p));
        }
        let mut id = 0;
        if !notify {
            st.ids += 1;
            id = st.ids;
            msg.insert("id".into(), json!(id));
        }
        let request = HttpRequest {
            method: "POST".into(),
            url: self.url.clone(),
            headers,
            body: Some(serde_json::to_vec(&Value::Object(msg)).unwrap_or_default()),
        };
        let (status, rheaders, content) = read_response(self.http.send(request).await?).await?;
        if let Some(s) = header(&rheaders, "mcp-session-id").filter(|s| !s.is_empty()) {
            st.session = Some(s.to_string());
        }
        if status == 404 && st.session.as_deref().is_some_and(|s| !s.is_empty()) {
            return Err(CallError::new(SESSION_GONE, ""));
        }
        if status == 401 || status == 403 {
            return Err(mcp_error(format!(
                "not signed in or not allowed (HTTP {status})"
            )));
        }
        if status >= 400 {
            let text = decode_utf8(&content);
            return Err(mcp_error(format!(
                "HTTP {status}: {}",
                take_chars(&text, 200)
            )));
        }
        if notify {
            return Ok(Map::new()); // 202 Accepted, no body (the transport spec)
        }
        let ctype = header(&rheaders, "content-type").unwrap_or("");
        let replies: Vec<Value> = if ctype.contains("text/event-stream") {
            sse_messages(&decode_utf8(&content))
                .into_iter()
                .map(Value::Object)
                .collect()
        } else if !content.is_empty() {
            vec![parse_json(&content)?]
        } else {
            Vec::new()
        };
        let mut reply = None;
        for m in replies {
            let Value::Object(m) = m else {
                return Err(CallError::new(
                    "AttributeError",
                    format!(
                        "'{}' object has no attribute 'get'",
                        eve_google_rest::py::type_name(&m)
                    ),
                ));
            };
            if id_matches(m.get("id"), id) {
                reply = Some(m);
                break;
            }
        }
        let Some(reply) = reply else {
            return Err(mcp_error(format!("no reply to {method}")));
        };
        if let Some(err) = reply.get("error") {
            let err = match err {
                Value::Object(e) => e.clone(),
                other => {
                    let mut e = Map::new();
                    e.insert("message".into(), json!(py_str(other)));
                    e
                }
            };
            let text = match err.get("message").filter(|m| truthy(m)) {
                Some(m) => py_str(m),
                None => py_repr(&Value::Object(err.clone())),
            };
            return Err(mcp_error(take_chars(&text, 300).to_string()));
        }
        match reply.get("result") {
            Some(Value::Object(r)) => Ok(r.clone()),
            _ => Ok(Map::new()),
        }
    }

    async fn open(&self, st: &mut McpState) -> Result<(), CallError> {
        st.session = None;
        st.version = PROTOCOL_VERSION.to_string();
        let params = json!({"protocolVersion": PROTOCOL_VERSION, "capabilities": {},
                            "clientInfo": {"name": "eve", "version": "1"}});
        let Value::Object(params) = params else {
            unreachable!("a json object literal")
        };
        let result = self.send(st, "initialize", Some(params), false).await?;
        st.version = match result.get("protocolVersion").filter(|v| truthy(v)) {
            Some(v) => py_str(v),
            None => PROTOCOL_VERSION.to_string(),
        };
        self.send(st, "notifications/initialized", None, true)
            .await?;
        st.ready = true;
        Ok(())
    }

    pub async fn request(
        &self,
        method: &str,
        params: Option<Map<String, Value>>,
    ) -> Result<Map<String, Value>, CallError> {
        let mut st = self.state.lock().await;
        for attempt in 1..=2 {
            if !st.ready {
                self.open(&mut st).await?;
            }
            match self.send(&mut st, method, params.clone(), false).await {
                Err(e) if e.kind == SESSION_GONE => {
                    st.ready = false;
                    if attempt == 2 {
                        return Err(mcp_error("the server keeps dropping the session"));
                    }
                }
                other => return other,
            }
        }
        Err(mcp_error("the server keeps dropping the session"))
    }

    pub async fn list_tools(&self) -> Result<Vec<Value>, CallError> {
        let mut tools = Vec::new();
        let mut cursor: Option<Value> = None;
        for _page in 0..20 {
            let mut params = Map::new();
            if let Some(c) = cursor.as_ref().filter(|c| truthy(c)) {
                params.insert("cursor".into(), c.clone());
            }
            let result = self.request("tools/list", Some(params)).await?;
            for t in py_iter(result.get("tools").filter(|v| truthy(v)))? {
                if let Value::Object(m) = &t {
                    if m.get("name").is_some_and(truthy) {
                        tools.push(t);
                    }
                }
            }
            cursor = result.get("nextCursor").cloned();
            if !cursor.as_ref().is_some_and(truthy) {
                break;
            }
        }
        Ok(tools)
    }

    pub async fn call_tool(
        &self,
        name: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Map<String, Value>, CallError> {
        let mut params = Map::new();
        params.insert("name".into(), json!(name));
        params.insert("arguments".into(), Value::Object(arguments.clone()));
        self.request("tools/call", Some(params)).await
    }
}

/// `for x in value` over a JSON value: a list's items; a dict's keys and a string's characters (none of them a
/// dict); a TypeError for anything else that is truthy. None or a falsy value is `[]`.
fn py_iter(v: Option<&Value>) -> Result<Vec<Value>, CallError> {
    match v {
        None => Ok(Vec::new()),
        Some(Value::Array(items)) => Ok(items.clone()),
        Some(Value::Object(m)) => Ok(m.keys().map(|k| json!(k)).collect()),
        Some(Value::String(s)) => Ok(s.chars().map(|c| json!(c.to_string())).collect()),
        Some(other) if !truthy(other) => Ok(Vec::new()),
        Some(other) => Err(CallError::new(
            "TypeError",
            format!(
                "'{}' object is not iterable",
                eve_google_rest::py::type_name(other)
            ),
        )),
    }
}

/// What the bridge needs of an app's client: McpClient's two calls (GoogleRestClient has the same two).
#[async_trait]
pub trait AppClient: Send + Sync {
    async fn list_tools(&self) -> Result<Vec<Value>, CallError>;
    async fn call_tool(
        &self,
        name: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Value, CallError>;
    /// The client when it is exactly Google's REST client (Python's `type(client) is GoogleRestClient`).
    fn google_rest(&self) -> Option<&GoogleRestClient> {
        None
    }
}

#[async_trait]
impl AppClient for McpClient {
    async fn list_tools(&self) -> Result<Vec<Value>, CallError> {
        McpClient::list_tools(self).await
    }

    async fn call_tool(
        &self,
        name: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Value, CallError> {
        McpClient::call_tool(self, name, arguments)
            .await
            .map(Value::Object)
    }
}

#[async_trait]
impl AppClient for GoogleRestClient {
    async fn list_tools(&self) -> Result<Vec<Value>, CallError> {
        Ok(GoogleRestClient::list_tools(self))
    }

    async fn call_tool(
        &self,
        name: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Value, CallError> {
        Ok(GoogleRestClient::call_tool(self, name, &Value::Object(arguments.clone())).await?)
    }

    fn google_rest(&self) -> Option<&GoogleRestClient> {
        Some(self)
    }
}

// ---- pure: names, schemas, decisions ----------------------------------------------------------------------

/// `<app>__<tool>` split, or ("", "") when the app is not one of SERVERS or the tool is empty.
pub fn split_name(name: &str) -> (&str, &str) {
    match name.split_once(SEP) {
        Some((app, tool)) if SERVERS.contains_key(app) && !tool.is_empty() => (app, tool),
        _ => ("", ""),
    }
}

fn words(tool: &str) -> Vec<String> {
    tool.replace('-', "_")
        .to_lowercase()
        .split('_')
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

fn writes(words: &[String]) -> bool {
    words.iter().enumerate().any(|(i, w)| {
        WRITE_VERBS.contains(&w.as_str()) && (!NOUN_TOO.contains(&w.as_str()) || i == 0)
    })
}

/// By its name alone: a reading verb and no writing one (a noun-too word writes only first).
pub fn name_reads(tool: &str) -> bool {
    let words = words(tool);
    words.iter().any(|w| READ_VERBS.contains(&w.as_str())) && !writes(&words)
}

/// A tool only looks when the server says so (readOnlyHint) and its name agrees. A server's hint alone never
/// makes `trash_thread` a read, and with no hint the tool is not a read: the name's words are a closed list, so
/// `find_and_replace` read by its name alone (Lean AppsPolicy P1, 2026-10-05).
pub fn tool_reads(tool: &Value) -> bool {
    let hint = match tool.get("annotations") {
        Some(Value::Object(a)) => a.get("readOnlyHint"),
        _ => None,
    };
    let name = tool
        .get("name")
        .filter(|v| truthy(v))
        .map(py_str)
        .unwrap_or_default();
    hint == Some(&Value::Bool(true)) && !writes(&words(&name))
}

/// `value in (None, False)` in Python: null, false, and a number equal to 0.
fn none_or_false(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) | Some(Value::Bool(false)) => true,
        Some(Value::Number(n)) => n.as_f64() == Some(0.0),
        _ => false,
    }
}

/// `value in (None, "", "primary")` (absent reads as "primary").
fn primary_calendar(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.is_empty() || s == "primary",
        _ => false,
    }
}

/// A closed list of reversible, private operations with understood arguments.
///
/// Tool names alone cannot establish privacy: a file in an existing/shared folder, a calendar edit with default
/// guest delivery, or a generic request body asks. Missing arguments (legacy callers and workbench scripts)
/// never prove a write routine. This is shared with Composio; it does not grant access to a tool or bypass
/// read/ask policy.
pub fn routine_write(app: &str, tool: &str, args: Option<&Value>) -> bool {
    let Some(Value::Object(args)) = args else {
        return false;
    };
    let mut normalized: IndexMap<String, &Value> = IndexMap::new();
    for (k, v) in args {
        let key: String = k
            .to_lowercase()
            .chars()
            .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            .collect();
        normalized.insert(key, v);
    }
    if normalized.len() != args.len() {
        // ambiguous aliases such as sendUpdates + send_updates
        return false;
    }
    let direct_calendar = app.to_lowercase() == "calendar";
    let lowered = app.to_lowercase();
    let app = match lowered.as_str() {
        "googlecalendar" => "calendar",
        "googledrive" => "drive",
        "googledocs" => "docs",
        "googlesheets" => "sheets",
        other => other,
    };
    let name = words(tool).join("_");
    let only = |fields: &[&str]| normalized.keys().all(|k| fields.contains(&k.as_str()));
    // Structured provider request bodies are deliberately outside this allowlist.
    let plain_values = || normalized.values().all(|v| !v.is_object());
    let get = |k: &str| normalized.get(k).copied();

    if app == "gmail"
        && [
            "create_draft",
            "update_draft",
            "create_email_draft",
            "update_email_draft",
            "draft_email",
            "draft_reply",
        ]
        .contains(&name.as_str())
    {
        return only(&[
            "draftid",
            "id",
            "threadid",
            "messageid",
            "replytomessageid",
            "to",
            "cc",
            "bcc",
            "recipient",
            "recipients",
            "subject",
            "body",
            "bodytext",
            "htmlbody",
            "ishtml",
            "attachmentids",
            "userid",
        ]) && plain_values();
    }
    if app == "drive" && ["create_file", "upload_file", "create_folder"].contains(&name.as_str()) {
        // A new item in the owner's root: no parent, existing file id, ACL or sharing options.
        return only(&[
            "title",
            "name",
            "textcontent",
            "content",
            "mimetype",
            "description",
            "filetoupload",
            "filename",
        ]) && plain_values();
    }
    if app == "docs" && name == "create_document" {
        return only(&["title"]) && plain_values();
    }
    if app == "sheets" && name == "create_spreadsheet" {
        return only(&["title"]) && plain_values();
    }
    if app == "github" && name == "create_repository" {
        // Use the actual adapter key: accepting an ignored alias could create a public repository.
        return args.get("private") == Some(&Value::Bool(true))
            && only(&["name", "description", "private", "autoinit"])
            && plain_values();
    }
    if app != "calendar" {
        return false;
    }
    let fields = [
        "summary",
        "title",
        "description",
        "location",
        "starttime",
        "endtime",
        "startdatetime",
        "enddatetime",
        "start",
        "end",
        "allday",
        "timezone",
        "recurrence",
        "addgooglemeeturl",
        "calendarid",
        "attendeeemails",
        "addattendeeemails",
        "attendees",
        "notifyguests",
        "sendupdates",
        "sendnotifications",
        "eventid",
    ];
    if name == "quick_add_event" {
        return only(&["text", "calendarid"])
            && matches!(get("text"), Some(Value::String(t)) if !t.contains('@'))
            && primary_calendar(get("calendarid"));
    }
    if ![
        "create_event",
        "update_event",
        "patch_event",
        "events_insert",
        "events_update",
        "events_patch",
    ]
    .contains(&name.as_str())
    {
        return false;
    }
    if !only(&fields) || !primary_calendar(get("calendarid")) {
        return false;
    }
    if ["attendeeemails", "addattendeeemails", "attendees"]
        .iter()
        .any(|k| get(k).is_some_and(truthy))
    {
        return false; // even silent additions share an event with other people
    }
    if !none_or_false(get("notifyguests")) || !none_or_false(get("sendnotifications")) {
        return false;
    }
    match get("sendupdates") {
        None | Some(Value::Null) => {}
        Some(Value::String(s)) if s == "none" => {}
        _ => return false,
    }
    for (key, value) in &normalized {
        if let Value::Object(m) = value {
            if !["start", "end"].contains(&key.as_str())
                || !m
                    .keys()
                    .all(|k| ["date", "dateTime", "timeZone"].contains(&k.as_str()))
            {
                return false;
            }
        }
    }
    if [
        "update_event",
        "patch_event",
        "events_update",
        "events_patch",
    ]
    .contains(&name.as_str())
    {
        // Existing attendees are not in the arguments. The direct adapter otherwise emails them by default.
        if direct_calendar {
            return args.get("notifyGuests") == Some(&Value::Bool(false));
        }
        return args.get("send_updates") == Some(&json!("none"));
    }
    true
}

/// `_clip`: whitespace squashed, then clipped to `limit` characters with "…".
pub fn clip(text: &str, limit: usize) -> String {
    let text = squash_ws(text);
    if text.chars().count() <= limit {
        return text;
    }
    format!("{}…", rstrip(take_chars(&text, limit.saturating_sub(1))))
}

/// `_clip(value, limit)` for a JSON value: `str(value or "")` first.
fn clip_value(v: Option<&Value>, limit: usize) -> String {
    clip(
        &v.filter(|v| truthy(v)).map(py_str).unwrap_or_default(),
        limit,
    )
}

/// An input schema the model needs: vendor extensions, titles and deprecated fields out, descriptions clipped.
pub fn slim_schema(schema: &Value) -> Value {
    match schema {
        Value::Array(items) => Value::Array(items.iter().map(slim_schema).collect()),
        Value::Object(m) => {
            let mut out = Map::new();
            for (k, v) in m {
                if k.starts_with("x-")
                    || ["deprecated", "title", "$schema", "examples"].contains(&k.as_str())
                {
                    continue;
                }
                if k == "description" {
                    out.insert(k.clone(), json!(clip_value(Some(v), PROP_CLIP)));
                } else if let (true, Value::Object(props)) = (k == "properties", v) {
                    let kept = props
                        .iter()
                        .filter(|(_, s)| !matches!(s, Value::Object(o) if o.get("deprecated").is_some_and(truthy)))
                        .map(|(p, s)| (p.clone(), slim_schema(s)))
                        .collect();
                    out.insert(k.clone(), Value::Object(kept));
                } else {
                    out.insert(k.clone(), slim_schema(v));
                }
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// One server tool as the brain's function tool: `<app>__<tool>` (64 characters at most), the app's label
/// before its clipped description, its slimmed input schema.
pub fn function_tool(app: &str, tool: &Value) -> Value {
    let mut params = slim_schema(
        tool.get("inputSchema")
            .filter(|v| truthy(v))
            .unwrap_or(&json!({})),
    );
    if params.get("type") != Some(&json!("object")) || !params.is_object() {
        params = json!({"type": "object", "properties": {}});
    }
    if let Value::Object(p) = &mut params {
        if !p.contains_key("properties") {
            p.insert("properties".into(), json!({}));
        }
    }
    let raw_name = tool.get("name").map(py_str).unwrap_or_default();
    let label = SERVERS.get(app).map(|s| s.label).unwrap_or(app);
    let desc = tool
        .get("description")
        .filter(|v| truthy(v))
        .or(tool.get("name"));
    json!({"type": "function", "name": take_chars(&format!("{app}{SEP}{raw_name}"), 64),
           "description": format!("[{label}] {}", clip_value(desc, DESC_CLIP)),
           "parameters": params, "strict": false})
}

/// What the bridge does with a call: run, ask the owner, or refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Run,
    Ask,
    Refuse,
}

impl Action {
    pub fn as_str(&self) -> &'static str {
        match self {
            Action::Run => "run",
            Action::Ask => "ask",
            Action::Refuse => "refuse",
        }
    }
}

impl PartialEq<&str> for Action {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl std::fmt::Display for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    /// run | ask | refuse
    pub action: Action,
    pub why: String,
}

impl Decision {
    pub fn new(action: Action, why: impl Into<String>) -> Self {
        Self {
            action,
            why: why.into(),
        }
    }
}

/// `_flat`: a string as it is, anything else as `json.dumps(ensure_ascii=False)`.
fn flat(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => dumps(other, false),
    }
}

fn describe_line(name: &str, args: &Map<String, Value>, value_clip: Option<usize>) -> String {
    let (app, tool) = split_name(name);
    let what = if app.is_empty() {
        name.to_string()
    } else {
        format!("{}: {}", SERVERS[app].label, tool.replace('_', " "))
    };
    let pairs = args
        .iter()
        .map(|(k, v)| match value_clip {
            None => format!("{k}: {}", flat(v)),
            Some(n) => format!("{k}: {}", clip(&flat(v), n)),
        })
        .collect::<Vec<_>>()
        .join(", ");
    if pairs.is_empty() {
        format!("{what}?")
    } else {
        format!("{what} with {pairs}?")
    }
}

/// One plain line the owner can answer yes or no to: "GitHub: create issue with repo: …, title: …?".
pub fn describe_for_confirmation(name: &str, args: &Map<String, Value>) -> String {
    let line = describe_line(name, args, Some(VALUE_CLIP));
    let n = line.chars().count();
    if n > LINE_CLIP {
        format!("{}?", clip(take_chars(&line, n - 1), LINE_CLIP - 1))
    } else {
        line
    }
}

/// The writes whose yes is asked on the whole act, never a clipped line, tainted or not: an email is read in
/// full before it goes (owner, 2026-10-09). The question can be this long.
pub const WHOLE_ASKS: [&str; 1] = ["gmail__send_message"];
pub const WHOLE_ASK_CHARS: usize = 3500;

pub fn asks_whole(name: &str) -> bool {
    WHOLE_ASKS.contains(&name)
}

/// An email as the owner reads it before saying yes: who, what, then the body as it will go.
fn email_preview(args: &Map<String, Value>) -> String {
    let mut lines = vec!["Send this email?".to_string()];
    for (key, label) in [("to", "To"), ("cc", "Cc"), ("bcc", "Bcc")] {
        if let Some(value) = args.get(key).filter(|v| truthy(v)) {
            let text = match value {
                Value::Array(items) => items.iter().map(py_str).collect::<Vec<_>>().join(", "),
                other => py_str(other),
            };
            lines.push(format!("{label}: {text}"));
        }
    }
    let replying = args.get("replyToMessageId").is_some_and(truthy);
    if replying {
        lines.push("In reply to the message eve read (same thread)".into());
    }
    let subject_raw = |default: &str| match args.get("subject").filter(|v| truthy(v)) {
        Some(v) => py_str(v),
        None => default.to_string(),
    };
    let subject = if replying {
        let s = strip(&subject_raw("")).to_string();
        if s.is_empty() {
            "(Re: the original's)".to_string()
        } else {
            s
        }
    } else {
        subject_raw("(none)")
    };
    lines.push(format!("Subject: {subject}"));
    let body = args
        .get("body")
        .filter(|v| truthy(v))
        .map(py_str)
        .unwrap_or_default();
    format!("{}\n\n{body}", lines.join("\n"))
}

/// The same line with nothing clipped, or None when it is longer than `limit`: a write a tool result may have
/// asked for is shown whole or not asked about at all (tool_gate; v3 safety review).
pub fn describe_whole(name: &str, args: &Map<String, Value>, limit: usize) -> Option<String> {
    // hidden and bidi characters shown as [U+XXXX], never as nothing (tool_gate::visible; round-4 review)
    if name == "gmail__send_message" {
        let text = eve_tool_gate::visible(&email_preview(args)); // line breaks kept: an email reads as one
        return (text.chars().count() <= limit).then_some(text);
    }
    let line = split_ws(&eve_tool_gate::visible(&describe_line(name, args, None))).join(" ");
    (line.chars().count() <= limit).then_some(line)
}

/// An MCP tools/call result as the brain reads it: the structured content when there is one, else the text.
///
/// A `content` that Python cannot iterate (a number or `true`) reads as no content here; `try_result_for_model`
/// is the form that fails there as Python's TypeError does, and `execute` uses it.
pub fn result_for_model(result: &Map<String, Value>) -> Map<String, Value> {
    match try_result_for_model(result) {
        Ok(out) => out,
        Err(_) => {
            let mut plain = result.clone();
            plain.remove("content");
            try_result_for_model(&plain).unwrap_or_default()
        }
    }
}

/// `result_for_model` with Python's TypeError for a `content` that is not iterable (`for c in 3`).
pub fn try_result_for_model(result: &Map<String, Value>) -> Result<Map<String, Value>, CallError> {
    let mut out = Map::new();
    let ok = !result.get("isError").is_some_and(truthy);
    out.insert("ok".into(), json!(ok));
    match result.get("structuredContent").filter(|v| !v.is_null()) {
        Some(structured) => {
            out.insert("result".into(), structured.clone());
        }
        None => {
            // `for c in result.get("content") or []`: a dict's keys and a string's characters are never dicts
            let content: Vec<&Map<String, Value>> = match result.get("content") {
                Some(Value::Array(items)) => items.iter().filter_map(Value::as_object).collect(),
                Some(other @ (Value::Number(_) | Value::Bool(true))) if truthy(other) => {
                    return Err(CallError::new(
                        "TypeError",
                        format!(
                            "'{}' object is not iterable",
                            eve_google_rest::py::type_name(other)
                        ),
                    ))
                }
                _ => Vec::new(),
            };
            let texts: Vec<String> = content
                .iter()
                .filter(|c| c.get("type") == Some(&json!("text")))
                .map(|c| {
                    c.get("text")
                        .filter(|v| truthy(v))
                        .map(py_str)
                        .unwrap_or_default()
                })
                .collect();
            out.insert("result".into(), json!(texts.join("\n")));
            let others: Vec<String> = content
                .iter()
                .filter(|c| c.get("type") != Some(&json!("text")))
                .map(|c| py_str(c.get("type").unwrap_or(&Value::Null)))
                .collect();
            if !others.is_empty() {
                out.insert("not_shown".into(), json!(others));
            }
        }
    }
    let raw = flat(&out["result"]);
    if raw.chars().count() > RESULT_CLIP {
        out.insert(
            "result".into(),
            json!(format!("{}…", take_chars(&raw, RESULT_CLIP))),
        );
        out.insert("clipped".into(), json!(true));
    }
    if !ok {
        out.insert("reason".into(), json!(clip(&raw, 300)));
    }
    Ok(out)
}

// ---- the bridge -------------------------------------------------------------------------------------------

/// Makes the MCP client of a server (GitHub, Maps) from its sign-in.
pub type ClientFactory =
    Arc<dyn Fn(&Server, Arc<dyn AppToken>) -> Result<Arc<dyn AppClient>, CallError> + Send + Sync>;
/// Makes the REST client of one of Google's apps from the Google sign-in.
pub type RestFactory =
    Arc<dyn Fn(&str, Arc<dyn AppToken>) -> Result<Arc<dyn AppClient>, CallError> + Send + Sync>;

/// The owner's connected apps, wrapped so nothing in here fails into the chat loop.
pub struct AppsBridge {
    pub config: AppsConfig,
    pub policy: Policy,
    factory: ClientFactory,
    token_sources: IndexMap<String, Arc<dyn AppToken>>,
    rest_factory: RestFactory,
    /// McpClient, or GoogleRestClient
    clients: IndexMap<String, Arc<dyn AppClient>>,
    /// app -> "mcp" | "rest", for the log and status
    pub paths: IndexMap<String, String>,
    /// function name -> the server's own tool
    raw: IndexMap<String, Value>,
    tools: Vec<Value>,
    /// the owner's GitHub repositories (github_repos)
    pub repos: Vec<github_repos::Repo>,
}

impl AppsBridge {
    /// `AppsBridge(config)`: real MCP and REST clients, and a TokenSource per sign-in kind.
    pub fn new(config: AppsConfig) -> Self {
        let policy = if config.policy.is_empty() {
            self::policy(Some(&Environ::new()))
        } else {
            config.policy.clone()
        };
        let timeout = finite_timeout(config.timeout_secs);
        let factory: ClientFactory = Arc::new(move |s: &Server, tok: Arc<dyn AppToken>| {
            let client = McpClient::new(
                s.url,
                SharedToken(tok),
                s.headers,
                timeout,
                key_header(s.auth),
            );
            Ok(Arc::new(client) as Arc<dyn AppClient>)
        });
        let rest_factory: RestFactory = Arc::new(move |app: &str, tok: Arc<dyn AppToken>| {
            let client = GoogleRestClient::new(app, SharedToken(tok), timeout)?;
            Ok(Arc::new(client) as Arc<dyn AppClient>)
        });
        let token_sources = ["google", "github", "maps"]
            .into_iter()
            .map(|kind| {
                (
                    kind.to_string(),
                    Arc::new(TokenSource::new(kind, config.clone())) as Arc<dyn AppToken>,
                )
            })
            .collect();
        Self {
            config,
            policy,
            factory,
            token_sources,
            rest_factory,
            clients: IndexMap::new(),
            paths: IndexMap::new(),
            raw: IndexMap::new(),
            tools: Vec::new(),
            repos: Vec::new(),
        }
    }

    /// The `client_factory=` seam.
    pub fn with_client_factory(mut self, factory: ClientFactory) -> Self {
        self.factory = factory;
        self
    }

    /// The `rest_factory=` seam.
    pub fn with_rest_factory(mut self, factory: RestFactory) -> Self {
        self.rest_factory = factory;
        self
    }

    /// The `tokens=` seam: these sign-ins instead of the TokenSources (an empty map: none at all).
    pub fn with_tokens(mut self, tokens: IndexMap<String, Arc<dyn AppToken>>) -> Self {
        self.token_sources = tokens;
        self
    }

    /// Put a client up by hand, as the Python tests set `_clients[app]` (and `paths[app]`).
    pub fn insert_client(&mut self, app: &str, client: Arc<dyn AppClient>, path: Option<&str>) {
        self.clients.insert(app.to_string(), client);
        if let Some(p) = path {
            self.paths.insert(app.to_string(), p.to_string());
        }
    }

    /// Register a server's own tool under a function name by hand, as the Python tests set `_raw[name]`.
    pub fn insert_raw(&mut self, name: &str, tool: Value) {
        self.raw.insert(name.to_string(), tool);
    }

    /// The apps whose sign-in is there.
    pub fn available(&self) -> Vec<String> {
        SERVERS
            .iter()
            .filter(|(_, server)| {
                self.token_sources
                    .get(server.auth)
                    .is_some_and(|src| src.available().unwrap_or(true))
            })
            .map(|(app, _)| app.to_string())
            .collect()
    }

    pub fn started(&self) -> bool {
        !self.clients.is_empty()
    }

    pub fn apps(&self) -> Vec<String> {
        self.clients.keys().cloned().collect()
    }

    /// (client, its tools, which path) for one app: Google's three over their plain APIs, the rest over MCP.
    async fn connect(
        &self,
        app: &str,
    ) -> Result<(Arc<dyn AppClient>, Vec<Value>, &'static str), CallError> {
        let server = &SERVERS[app];
        let token = self.token_sources[server.auth].clone();
        if server.auth == "google" {
            let client = (self.rest_factory)(app, token)?;
            let listed = client.list_tools().await?;
            return Ok((client, listed, "rest"));
        }
        let client = (self.factory)(server, token)?;
        let listed = client.list_tools().await?;
        Ok((client, listed, "mcp"))
    }

    /// List each signed-in app's tools. An app that fails is left out with one log line.
    pub async fn start(&mut self) {
        for app in self.available() {
            let server = &SERVERS[app.as_str()];
            let (client, listed, path) = match self.connect(&app).await {
                Ok(c) => c,
                Err(e) => {
                    // the type and a short reason; never a token
                    tracing::warn!(
                        "apps: {} is not available ({}: {})",
                        server.label,
                        e.type_name(),
                        clip(&e.message, 160)
                    );
                    continue;
                }
            };
            self.paths.insert(app.clone(), path.to_string());
            let mut offered = 0;
            for tool in &listed {
                if tool.get("name").is_none_or(Value::is_null) {
                    continue; // no name to call it by
                }
                if self.policy.get(&app).map(String::as_str) == Some("read") && !tool_reads(tool) {
                    continue; // refused anyway: not offered, not paid for each turn
                }
                let fn_tool = function_tool(&app, tool);
                let name = fn_tool["name"].as_str().unwrap_or_default().to_string();
                self.raw.insert(name, tool.clone());
                self.tools.push(fn_tool);
                offered += 1;
            }
            self.clients.insert(app.clone(), client);
            tracing::info!(
                "apps: {} up ({}), {} of {} tools offered ({})",
                server.label,
                path,
                offered,
                listed.len(),
                self.policy.get(&app).map(String::as_str).unwrap_or("None")
            );
        }
        if self.clients.contains_key("github") {
            self.load_repos().await;
        }
    }

    /// The owner's repositories, kept a day (github_repos::refresh); their names become GitHub words.
    async fn load_repos(&mut self) {
        let path = expanduser(&self.config.config_dir).join(github_repos::FILE);
        let repos = match github_repos::refresh(&path, &*self, &now).await {
            Ok(r) => r,
            Err(e) => {
                // a list that cannot be had costs the line, never the apps
                tracing::warn!("apps: GitHub repositories not loaded ({})", e.type_name());
                Vec::new()
            }
        };
        self.repos = repos;
        set_repo_names(github_repos::keyword_names(&self.repos));
    }

    pub fn tools(&self) -> Vec<Value> {
        self.tools.clone()
    }

    /// The up apps a message names (names_app).
    pub fn guess(&self, text: &str) -> HashSet<String> {
        self.clients
            .keys()
            .filter(|app| names_app(app, text))
            .cloned()
            .collect()
    }

    pub fn open_tool(&self, already: &HashSet<String>) -> Option<Value> {
        let closed: Vec<(&str, &str)> = self
            .clients
            .keys()
            .filter(|app| !already.contains(*app))
            .map(|app| {
                (
                    app.as_str(),
                    SERVERS
                        .get(app.as_str())
                        .map(|s| s.label)
                        .unwrap_or(app.as_str()),
                )
            })
            .collect();
        open_tool_spec(&closed)
    }

    /// One turn's app tools: open_app while some app is still closed, then the opened apps' tools.
    pub fn tools_for(&self, opened: &HashSet<String>) -> Vec<Value> {
        let mut out: Vec<Value> = self.open_tool(opened).into_iter().collect();
        out.extend(
            self.tools
                .iter()
                .filter(|t| opened.contains(split_name(t["name"].as_str().unwrap_or("")).0))
                .cloned(),
        );
        out
    }

    /// open_app: adds each app named (`apps`, or the older single `app`) to `opened` (the turn's set) and names
    /// their tools. An app that is not up is reported beside the ones opened, never failing them.
    pub fn open(
        &self,
        args: &Map<String, Value>,
        opened: &mut HashSet<String>,
    ) -> Map<String, Value> {
        let wanted = open_asked(args);
        let good: Vec<String> = wanted
            .iter()
            .filter(|a| self.clients.contains_key(*a))
            .cloned()
            .collect();
        let unknown: Vec<String> = wanted
            .iter()
            .filter(|a| !self.clients.contains_key(*a))
            .cloned()
            .collect();
        let connected = self.clients.keys().cloned().collect::<Vec<_>>().join(", ");
        let mut out = Map::new();
        if good.is_empty() {
            let who = if unknown.is_empty() {
                "no app".to_string()
            } else {
                unknown.join(", ")
            };
            out.insert("ok".into(), json!(false));
            out.insert(
                "reason".into(),
                json!(format!("{who} is not connected; connected: {connected}")),
            );
            return out;
        }
        opened.extend(good.iter().cloned());
        let tools: Vec<&str> = self
            .tools
            .iter()
            .filter_map(|t| t["name"].as_str())
            .filter(|n| good.iter().any(|g| g == split_name(n).0))
            .collect();
        out.insert("ok".into(), json!(true));
        out.insert("opened".into(), json!(good));
        out.insert("tools".into(), json!(tools));
        if !unknown.is_empty() {
            out.insert("not_connected".into(), json!(unknown));
            out.insert(
                "note".into(),
                json!(format!(
                    "{} is not connected; connected: {connected}",
                    unknown.join(", ")
                )),
            );
        }
        out
    }

    /// Whether a function name is this bridge's to answer: `<app>__<tool>` for an app that is up (a tool it lacks
    /// too, so decide can say which it has), or open_app while some app is up.
    pub fn owns(&self, name: &str) -> bool {
        if name == OPEN_TOOL {
            return !self.clients.is_empty();
        }
        self.clients.contains_key(split_name(name).0)
    }

    /// A call to an up app's tool opens that app for the rest of the turn (the model named it; it gets its tools
    /// next round, and the call itself runs through decide like any other).
    pub fn ensure_open(&self, name: &str, opened: &mut HashSet<String>) {
        let app = split_name(name).0;
        if self.clients.contains_key(app) {
            opened.insert(app.to_string());
        }
    }

    fn tool_names(&self, app: &str) -> Vec<String> {
        self.tools
            .iter()
            .filter_map(|t| t["name"].as_str())
            .filter(|n| split_name(n).0 == app)
            .map(str::to_string)
            .collect()
    }

    pub fn names(&self) -> BTreeSet<String> {
        let mut out: BTreeSet<String> = self.raw.keys().cloned().collect();
        if !self.clients.is_empty() {
            out.insert(OPEN_TOOL.to_string());
        }
        out
    }

    pub fn reads(&self, name: &str) -> bool {
        self.raw.get(name).is_some_and(tool_reads)
    }

    /// Copy a call, pinning no delivery for a verified guest-free direct calendar edit.
    ///
    /// This read is code-owned and never changes the input or the adapter's sending defaults. Pinning the actual
    /// flag also prevents guest delivery if guests appear between the read and patch.
    pub async fn prepare(&self, name: &str, args: &Map<String, Value>) -> Map<String, Value> {
        let prepared = args.clone();
        let client = self.clients.get("calendar").and_then(|c| c.google_rest());
        let event_id = match prepared.get("eventId") {
            Some(Value::String(s)) if !strip(s).is_empty() => s.clone(),
            _ => return prepared,
        };
        let Some(client) = client.filter(|c| c.app == "calendar") else {
            return prepared;
        };
        if name != "calendar__update_event"
            || !self.raw.contains_key(name)
            || !matches!(
                self.policy.get("calendar").map(String::as_str),
                Some("auto" | "write")
            )
            || prepared.get("notifyGuests").is_some_and(|v| !v.is_null())
        {
            return prepared;
        }
        let mut silent = prepared.clone();
        silent.insert("notifyGuests".into(), json!(false));
        if !routine_write(
            "calendar",
            "update_event",
            Some(&Value::Object(silent.clone())),
        ) {
            return prepared;
        }
        // get_event compacts away attendeesOmitted, so inspect the full resource without a field mask.
        let url = format!(
            "{}/{}",
            GoogleRestClient::cal(&prepared),
            url_quote(&event_id)
        );
        let event = match client.req("GET", &url, &Vec::new(), Body::None).await {
            Ok(e) => e,
            Err(e) => {
                // a failed inspection retains the original ask, never content in logs
                tracing::info!("apps: calendar preparation failed: {}", e.type_name());
                return prepared;
            }
        };
        if let Value::Object(event) = &event {
            let attendees_empty = match event.get("attendees") {
                None => true,
                Some(Value::Array(a)) => a.is_empty(),
                _ => false,
            };
            if event.get("id") == Some(&json!(event_id))
                && event.get("status") != Some(&json!("cancelled"))
                && event.get("attendeesOmitted").unwrap_or(&json!(false)) == &json!(false)
                && attendees_empty
            {
                return silent;
            }
        }
        prepared
    }

    /// Reads run; explicit read/ask policies remain strict; only understood routine writes run unasked.
    pub fn decide(&self, name: &str, args: Option<&Value>) -> Decision {
        let (app, tool) = split_name(name);
        if !self.raw.contains_key(name) && self.clients.contains_key(app) {
            // A name the app does not have, or a change a read-only app never offered: the reason names its real
            // tools, so the next round can call the right one instead of guessing again.
            let names = self.tool_names(app);
            let have = if names.is_empty() {
                "none".to_string()
            } else {
                names[..names.len().min(60)].join(", ")
            };
            let label = SERVERS[app].label;
            if self.policy.get(app).map(String::as_str) == Some("read") {
                return Decision::new(
                    Action::Refuse,
                    format!("{label} is read only here; {name} will not run. Its tools: {have}"),
                );
            }
            return Decision::new(
                Action::Refuse,
                format!("{name} is not one of {label}'s tools. Its tools: {have}"),
            );
        }
        if !self.raw.contains_key(name) || app.is_empty() {
            return Decision::new(
                Action::Refuse,
                format!("{name} is not one of the connected apps' tools"),
            );
        }
        if self.reads(name) {
            return Decision::new(Action::Run, "");
        }
        let rule = self.policy.get(app).map(String::as_str).unwrap_or("ask");
        if rule == "read" {
            return Decision::new(
                Action::Refuse,
                format!(
                    "{} is read only here; {name} will not run",
                    SERVERS[app].label
                ),
            );
        }
        let run = matches!(rule, "auto" | "write") && routine_write(app, tool, args);
        Decision::new(if run { Action::Run } else { Action::Ask }, "")
    }

    /// Never fails: a failure is a map the model can read and tell the owner about.
    pub async fn execute(&self, name: &str, args: &Map<String, Value>) -> Map<String, Value> {
        let (app, _tool) = split_name(name);
        let (Some(client), Some(raw)) = (self.clients.get(app), self.raw.get(name)) else {
            let mut out = Map::new();
            out.insert("ok".into(), json!(false));
            out.insert("reason".into(), json!(format!("{name} is not available")));
            return out;
        };
        let raw_name = raw.get("name").map(py_str).unwrap_or_default();
        let failed = |kind: &str, message: &str| {
            // the type only: arguments may hold mail text
            tracing::warn!("apps: {name} failed: {kind}");
            let mut out = Map::new();
            out.insert("ok".into(), json!(false));
            out.insert(
                "reason".into(),
                json!(format!("{kind}: {}", clip(message, 200))),
            );
            out
        };
        match client.call_tool(&raw_name, args).await {
            Ok(Value::Object(result)) => match try_result_for_model(&result) {
                Ok(out) => out,
                Err(e) => failed(e.type_name(), &e.message),
            },
            Ok(other) => failed(
                "AttributeError",
                &format!(
                    "'{}' object has no attribute 'get'",
                    eve_google_rest::py::type_name(&other)
                ),
            ),
            Err(e) => failed(e.type_name(), &e.message),
        }
    }

    /// For the brain, only while some app is up: which apps, what may be changed in each, and how to reach them
    /// fast. It rides every turn's prompt, so it is short. Owner's rule (2026-10-04): never write a wrong tool
    /// *name* here, even as a warning; every name below is one this bridge offers when it is written.
    pub fn instructions(&self) -> String {
        if self.clients.is_empty() {
            return String::new();
        }
        let routine = "routine private changes run; sending, deleting, sharing, publishing and purchases are asked by code";
        let rule_text = |rule: &str| match rule {
            "read" => "read only (you may look, never change anything; say so if asked)",
            "auto" | "write" => routine,
            _ => "changes are asked of the owner first, by code",
        };
        let mut lines = vec![
            "Your owner's own apps are reachable through their tools, named app__tool. A job inside the owner's own \
             mail, calendar, Drive, GitHub, or places, routes and weather is done with these tools, which answer in \
             seconds, rather than a task on the Mac's screen or a web search. An app whose tools you do not have \
             yet is opened first, every app the request needs in one call."
                .to_string(),
        ];
        for app in self.clients.keys() {
            let label = SERVERS
                .get(app.as_str())
                .map(|s| s.label)
                .unwrap_or(app.as_str());
            let rule = self.policy.get(app).map(String::as_str).unwrap_or("ask");
            lines.push(format!("- {label} ({app}__…): {}", rule_text(rule)));
        }
        if self.clients.contains_key("github") && !self.repos.is_empty() {
            lines.push(github_repos::line(&self.repos));
        }
        lines.push("Chain apps in the same turn without asking in between.".into());
        if self.clients.contains_key("calendar") {
            lines.push(
                "\"My next class\" (or meeting, flight) is the calendar's event list from now, with no calendar \
                 named: it reads every calendar the owner sees in one call, and the answer is the first upcoming \
                 event of that kind. An event with a place already carries mapsUrl and directionsUrl, so a Maps \
                 link for it needs no maps call."
                    .into(),
            );
        }
        if self.clients.contains_key("gmail")
            && self.policy.get("gmail").map(String::as_str) != Some("read")
        {
            lines.push(
                "Gmail sends email, written in the owner's voice; the owner sees each one whole and says yes by \
                 code. An answer to a message goes into its thread."
                    .into(),
            );
        }
        if self.clients.contains_key("contacts") {
            lines.push(
                "Someone's email address or phone number that you do not have comes from Google Contacts; never \
                 guess an address."
                    .into(),
            );
        }
        if self.clients.contains_key("maps") {
            lines.push(
                "The maps tools are for travel time, nearby places and weather. A place, route or weather from them \
                 is followed by its Google Maps link, as Google's terms ask."
                    .into(),
            );
        }
        lines.push(
            "Keep the links that results carry (event, map, file, PR) in your reply. You do not need to ask before \
             a change that is asked by code. Summarise what came back in your own few words; never paste a raw \
             record. Anything on the public web is a web search."
                .into(),
        );
        lines.join("\n")
    }
}

#[async_trait]
impl github_repos::Call for AppsBridge {
    async fn call(&self, name: &str, args: &Map<String, Value>) -> Result<Value, CallError> {
        Ok(Value::Object(self.execute(name, args).await))
    }
}

/// `urllib.parse.quote(text, safe="")`.
fn url_quote(text: &str) -> String {
    urlencoding::encode(text).into_owned()
}

// ---- one-time sign-ins (`apps login`) ---------------------------------------------------------------------

fn token_urlsafe(nbytes: usize) -> String {
    use rand::RngCore;
    let mut bytes = vec![0u8; nbytes];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// `_pkce`: (verifier, S256 challenge).
pub fn pkce() -> (String, String) {
    let verifier = token_urlsafe(64);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

/// `parse_qsl(query)` into a dict: blank values dropped, the last value of a key kept.
fn parse_qs(query: &str) -> HashMap<String, String> {
    url::form_urlencoded::parse(query.as_bytes())
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

/// One request on the loopback server: the query of its path, and a plain-text answer.
async fn handle_one(
    mut conn: tokio::net::TcpStream,
    state: &str,
    got: &mut HashMap<String, String>,
) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match tokio::time::timeout(Duration::from_secs(30), conn.read(&mut chunk)).await {
            Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break,
            Ok(Ok(n)) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n")
                    || buf.windows(2).any(|w| w == b"\n\n")
                    || buf.len() > 65536
                {
                    break;
                }
            }
        }
    }
    let head = String::from_utf8_lossy(&buf);
    let first = head.lines().next().unwrap_or("");
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");
    let date = chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S GMT");
    if method != "GET" {
        let reply = format!(
            "HTTP/1.0 501 Unsupported method ({})\r\nDate: {date}\r\nConnection: close\r\nContent-Type: text/html;charset=utf-8\r\n\r\n",
            eve_google_rest::py::repr_str(method)
        );
        let _ = conn.write_all(reply.as_bytes()).await;
        return;
    }
    let path = target.split('#').next().unwrap_or("");
    let query = path.split_once('?').map(|(_, q)| q).unwrap_or("");
    let q = parse_qs(query);
    if q.get("state").map(String::as_str) == Some(state) {
        got.extend(q.clone());
    }
    let body: &[u8] = if q.get("code").is_some_and(|c| !c.is_empty()) {
        b"eve: signed in. You can close this tab."
    } else {
        b"eve: no sign-in code here."
    };
    let mut reply = format!(
        "HTTP/1.0 200 OK\r\nDate: {date}\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n"
    )
    .into_bytes();
    reply.extend_from_slice(body);
    let _ = conn.write_all(&reply).await;
    let _ = conn.shutdown().await;
}

/// The installed-app flow with PKCE on a loopback port: the owner's browser signs in, Google redirects to
/// 127.0.0.1, the refresh token is stored 0600. Returns the scopes granted. `port` 0 picks a free one.
pub async fn google_login(
    config: &AppsConfig,
    open_url: &(dyn Fn(&str) + Send + Sync),
    port: u16,
    say: &(dyn Fn(&str) + Send + Sync),
) -> anyhow::Result<String> {
    google_login_with(
        config,
        open_url,
        port,
        say,
        Arc::new(ReqwestTransport::new(30.0)),
    )
    .await
}

/// `google_login` with the token exchange over a given transport.
pub async fn google_login_with(
    config: &AppsConfig,
    open_url: &(dyn Fn(&str) + Send + Sync),
    port: u16,
    say: &(dyn Fn(&str) + Send + Sync),
    http: Arc<dyn Transport>,
) -> anyhow::Result<String> {
    let client = google_client(&config.google_client_path()).map_err(|e| anyhow::anyhow!("{e}"))?;
    let (verifier, challenge) = pkce();
    let state = token_urlsafe(16);
    let mut got: HashMap<String, String> = HashMap::new();

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let redirect = format!("http://127.0.0.1:{}/", listener.local_addr()?.port());
    let pol = if config.policy.is_empty() {
        policy(Some(&Environ::new()))
    } else {
        config.policy.clone()
    };
    let scopes = google_scopes(&pol);
    let client_id = form_value(client.get("client_id").unwrap_or(&Value::Null));
    let client_secret = form_value(client.get("client_secret").unwrap_or(&json!("")));
    let url = format!(
        "{GOOGLE_AUTH_URL}?{}",
        urlencode(&[
            // urlencode writes str() of a value; httpx's form below writes true/false/"" for a bool or None
            (
                "client_id",
                py_str(client.get("client_id").unwrap_or(&Value::Null))
            ),
            ("redirect_uri", redirect.clone()),
            ("response_type", "code".into()),
            ("scope", scopes.join(" ")),
            ("access_type", "offline".into()),
            ("prompt", "consent".into()),
            ("state", state.clone()),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256".into()),
        ])
    );
    say(&format!(
        "Opening Google sign-in in your browser. If it does not open, visit:\n{url}"
    ));
    open_url(&url);
    let deadline = now() + 300.0;
    let filled = |got: &HashMap<String, String>, k: &str| got.get(k).is_some_and(|v| !v.is_empty());
    while !filled(&got, "code") && !filled(&got, "error") && now() < deadline {
        // handle_request: one connection, waiting up to the server's 300 s timeout for it
        if let Ok(Ok((conn, _))) =
            tokio::time::timeout(Duration::from_secs(300), listener.accept()).await
        {
            handle_one(conn, &state, &mut got).await;
        }
    }
    drop(listener);
    let Some(code) = got.get("code").filter(|c| !c.is_empty()).cloned() else {
        let why = got
            .get("error")
            .filter(|e| !e.is_empty())
            .cloned()
            .unwrap_or_else(|| "timed out".into());
        anyhow::bail!("no sign-in code ({why})");
    };
    let form = urlencode(&[
        ("grant_type", "authorization_code".into()),
        ("code", code),
        ("redirect_uri", redirect),
        ("client_id", client_id),
        ("client_secret", client_secret),
        ("code_verifier", verifier),
    ]);
    let request = HttpRequest {
        method: "POST".into(),
        url: GOOGLE_TOKEN_URL.into(),
        headers: vec![(
            "Content-Type".into(),
            "application/x-www-form-urlencoded".into(),
        )],
        body: Some(form.into_bytes()),
    };
    let r = http
        .send(request)
        .await
        .map_err(|e| anyhow::anyhow!("{}: {}", e.kind, e.message))?;
    let (status, _, content) = read_response(r)
        .await
        .map_err(|e| anyhow::anyhow!("{}: {e}", e.kind))?;
    let body = parse_json(&content).map_err(|e| anyhow::anyhow!("{}: {e}", e.kind))?;
    let Value::Object(body) = body else {
        anyhow::bail!(
            "'{}' object has no attribute 'get'",
            eve_google_rest::py::type_name(&body)
        );
    };
    let Some(refresh) = body.get("refresh_token").filter(|v| truthy(v)).cloned() else {
        let why = body
            .get("error")
            .filter(|v| truthy(v))
            .map(py_str)
            .unwrap_or_else(|| status.to_string());
        anyhow::bail!("Google gave no refresh token ({why})");
    };
    let expires_in = py_float(
        body.get("expires_in")
            .filter(|v| truthy(v))
            .unwrap_or(&json!(3600)),
    )
    .map_err(|e| anyhow::anyhow!("{}: {e}", e.kind))?;
    let scope = body.get("scope").cloned().unwrap_or(json!(""));
    let mut saved = Map::new();
    saved.insert("refresh_token".into(), refresh);
    saved.insert(
        "access_token".into(),
        body.get("access_token").cloned().unwrap_or(json!("")),
    );
    saved.insert("expires_at".into(), json!(now() + expires_in));
    saved.insert("scope".into(), scope.clone());
    // the 7-day "Testing" clock starts here (refresh reminds before it ends)
    saved.insert("signed_in_at".into(), json!(now()));
    write_private_json(&config.google_token_path(), &saved)?;
    // The owner may untick a permission on Google's page; what was not granted is said, not discovered later.
    let scope_text = py_str(&scope);
    let granted: HashSet<&str> = split_ws(&scope_text).into_iter().collect();
    let missing: BTreeSet<&String> = scopes
        .iter()
        .filter(|s| !granted.contains(s.as_str()))
        .collect();
    if !missing.is_empty() {
        say(&format!(
            "Not granted (those tools will fail until you sign in again and allow them): {}",
            missing.into_iter().cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    Ok(py_str(&scope))
}
