//! eve lite's apps: Composio Connect's meta tools over MCP, signed in as the person (docs/plans/eve-lite.md,
//! WP-6).
//!
//! Owner, 2026-10-13: "use composio and openrouter as the only setup things". eve lite signs in to Composio
//! Connect in the browser (onboarding.ComposioConnectLogin writes `composio-connect.json`), and the brain then
//! reaches the person's apps through Connect's MCP server, `https://connect.composio.dev/mcp`. Spike S1
//! (2026-10-13, docs/plans/eve-lite-s1.md) measured what this module relies on; its captures are
//! bridge/tests/fixtures/composio_connect.
//!
//! * [`ConnectTokenSource`] hands out the access token in the token file. It refreshes the token about a minute
//!   before it lapses, and once more when Composio answers 401: a form POST to the token endpoint with
//!   `grant_type`, `refresh_token`, `client_id` and `resource`, and no secret (the form S1 verified). The answer is
//!   written back (0600, atomically) with its refresh token, because the server may rotate it. A refresh token the
//!   token endpoint refuses (`invalid_grant`, an unknown client) is never sent again: the person signs in again
//!   from Settings → Apps. A 401 from /mcp to the fresh token too says less: S1's 401 also covers Composio not
//!   finding the account, which may pass. That sign-in is held for [`REFUSED_RETRY_SECS`] (`hold`), then tried
//!   again.
//! * [`ConnectBridge`] is the duck type the brain already reads for Composio (eve_composio_tools::ComposioBridge:
//!   `started`, `tools()`, `names`, `execute`, `accounts_note`), over `eve_apps_mcp::McpClient`. It offers the
//!   seven meta tools ([`META_TOOLS`]) under their own `COMPOSIO_*` names, so the code gate
//!   (eve_composio_tools::decide_lite) judges every call as it judges full eve's. A tool the server lists beyond
//!   those seven is left out: the gate runs a name it does not know without asking.
//! * What the model reads: each input schema slimmed (`eve_apps_mcp::slim_schema`), and Composio's own description
//!   whole, less its catalog line ([`CATALOG_LINE`]). The descriptions carry the meta tools' protocol (sessions,
//!   schemas, the sign-in link to send); apps_mcp's 400-character clip would keep COMPOSIO_SEARCH_TOOLS's catalog
//!   line and cut all of that. Full eve's SDK passes them whole too.
//! * `execute` never fails: a failure is `{"ok": false, "reason": ...}`. Composio's answer is read into its own
//!   object (`data`, `error`, `successful`) with `ok` added, the shape full eve's bridge returns. A 401 refreshes
//!   the sign-in once and tries once more, then answers plainly. After a 401 to the fresh token as well, no call
//!   sends the sign-in or mints a token pair until its hold is over, and the model is told to try again later.
//!   The model's line, and Settings, ask for a new sign-in only when this one cannot be renewed: the token endpoint
//!   refused it, or it has no refresh token and its access token is refused or has lapsed. Nothing else is
//!   retried: a call that timed out may have run.
//! * `start()` returns at once. A task lists the tools, trying again with backoff ([`START_BACKOFF`]) until it
//!   works or `close()`. Until then `started` is false and the brain offers no app tools.
//! * Settings → Apps (lite_web) has `connect_link(toolkit)`, the Connect Link that connects one app (read from that
//!   app's own part of Composio's answer, never another app's; `https` on composio.dev or a subdomain, else None),
//!   and `connections()`, which apps are connected. That is read from COMPOSIO_SEARCH_TOOLS, which only looks:
//!   COMPOSIO_MANAGE_CONNECTIONS starts a sign-in for each app it is asked about that is not connected, so it runs
//!   only when the person asks for a link.
//!
//! Tokens never reach a log, a Debug, a result or the model, and logs carry tool names, counts and error types
//! only: arguments and results may hold mail text (AGENTS rule 10). Unlike onboarding's, these tokens are not
//! handed to `telegram::hide_token`: an access token is replaced many times a day, and hide_token's set only
//! grows.

use std::collections::BTreeSet;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt as _;
use indexmap::IndexMap;
use parking_lot::Mutex;
use regex::Regex;
use serde_json::{json, Map, Value};
use sha2::{Digest as _, Sha256};

use eve_apps_mcp::py::{dumps, expanduser, json_loads, urlencode};
use eve_apps_mcp::{write_private_json, CallError, McpClient, MCP_ERROR};
use eve_brain::onboarding::{
    answer as token_answer, expires_at as token_expires_at, oauth_word, CONNECT_ISSUER, CONNECT_RESOURCE,
    CONNECT_TOKEN_FILE, CONNECT_TOKEN_URL, TOKEN_FILE_VERSION, TOKEN_SHAPE,
};
use eve_google_rest::py::{py_str, rstrip, splitlines, strip, take_chars, truthy};
use eve_google_rest::{HttpRequest, HttpResponse, Transport, TransportError};

/// A JSON object, as Python's dict.
pub type Obj = Map<String, Value>;

/// A clock in seconds: `time.time` for the token source, `time.monotonic` for the bridge.
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

/// `write_private_json`, as the token source saves a refreshed sign-in (a seam: the tests make the disk full).
pub type Writer = Arc<dyn Fn(&Path, &Obj) -> std::io::Result<()> + Send + Sync>;

/// Connect's MCP endpoint is also the resource its tokens are minted for (S1).
pub const MCP_URL: &str = CONNECT_RESOURCE;
pub const SEARCH_TOOLS: &str = "COMPOSIO_SEARCH_TOOLS";
pub const MANAGE_CONNECTIONS: &str = "COMPOSIO_MANAGE_CONNECTIONS";
/// The seven meta tools Composio Connect serves (S1: the same seven its docs list), in the order they are offered.
/// The order is fixed, not the server's, so the prompt's cached prefix is the same from one start to the next.
pub const META_TOOLS: [&str; 7] = [
    SEARCH_TOOLS,
    "COMPOSIO_GET_TOOL_SCHEMAS",
    "COMPOSIO_MULTI_EXECUTE_TOOL",
    MANAGE_CONNECTIONS,
    "COMPOSIO_WAIT_FOR_CONNECTIONS",
    "COMPOSIO_REMOTE_WORKBENCH",
    "COMPOSIO_REMOTE_BASH_TOOL",
];
/// COMPOSIO_SEARCH_TOOLS's description opens with a line selling Composio's catalog, its web search and a browser
/// among it. eve never runs those toolkits (eve_composio_tools::WEB_TOOLKITS: the public web is eve's own
/// web_search), so the line is left out rather than naming tools no turn can run (the owner's rule for prompts).
/// The rest is Composio's.
pub const CATALOG_LINE: &str = "Tool Server Info:";
/// a longer description is clipped (the longest S1 saw: 4,363 characters)
pub const DESC_MAX: usize = 8000;

/// the brain's calls: a workbench or bash cell may run 180 s (their descriptions)
pub const CALL_TIMEOUT_SECS: f64 = 200.0;
/// Settings → Apps' calls: a Connect Link; which apps are connected
pub const OWN_TIMEOUT_SECS: f64 = 60.0;
pub const CONNECT_TIMEOUT_SECS: f64 = 15.0;
pub const TOKEN_TIMEOUT_SECS: f64 = 20.0;
/// a token with less left than this is refreshed before it is sent
pub const REFRESH_EARLY_SECS: f64 = 60.0;
/// no refresh this soon after one that failed for a reason that may pass
pub const REFRESH_RETRY_SECS: f64 = 30.0;
/// how long a sign-in /mcp refused even fresh from a refresh is held (hold)
pub const REFUSED_RETRY_SECS: f64 = 300.0;
/// between start attempts; the last repeats
pub const START_BACKOFF: [f64; 6] = [5.0, 15.0, 30.0, 60.0, 120.0, 300.0];
/// how long connections() trusts what it last read
pub const CONNECTIONS_TTL_SECS: f64 = 60.0;
/// no new read this soon after one that failed
pub const LOOK_RETRY_SECS: f64 = 15.0;
/// apps per COMPOSIO_SEARCH_TOOLS call when connections() reads them
pub const LOOK_MAX: usize = 10;

/// Settings → Apps (docs/plans/eve-lite.md, flow A step 6): the apps it offers a Connect button for, by Composio
/// toolkit slug, in its order, with the name it shows. Any other toolkit with managed sign-in connects too.
pub const APPS: [(&str, &str); 7] = [
    ("gmail", "Gmail"),
    ("googlecalendar", "Google Calendar"),
    ("googledrive", "Google Drive"),
    ("slack", "Slack"),
    ("notion", "Notion"),
    ("linear", "Linear"),
    ("github", "GitHub"),
];

/// [`APPS`]' slugs, in order (Python's `tuple(APPS)`).
pub fn app_slugs() -> Vec<&'static str> {
    APPS.iter().map(|(slug, _)| *slug).collect()
}

/// What connections() asks COMPOSIO_SEARCH_TOOLS for each app: one plain read in it, by name, so the answer's
/// toolkit_connection_statuses reports that app. The search only looks: it finds tools and reports each app's
/// connection, and changes nothing in the app.
const LOOKS: [(&str, &str); 7] = [
    ("gmail", "read my latest emails in Gmail"),
    ("googlecalendar", "list my upcoming events in Google Calendar"),
    ("googledrive", "list my recent files in Google Drive"),
    ("slack", "list my channels in Slack"),
    ("notion", "search my pages in Notion"),
    ("linear", "list my assigned issues in Linear"),
    ("github", "list my repositories on GitHub"),
];

// What the model reads when a call cannot be made. Plain lines it can pass on.
pub const NOT_UP: &str = "Composio is not reachable from this Mac yet, so the apps cannot be used. Try again in a minute.";
pub const SIGNED_OUT: &str = "The Composio sign-in on this Mac has lapsed, so the apps cannot be used. The owner signs in to Composio again in Settings → Apps.";
pub const NOT_RENEWED: &str = "Composio did not take the sign-in, and it could not be renewed just now, so the apps cannot be used. Try again in a few minutes.";
pub const MAY_HAVE_RUN: &str = " The call may have run: check before running it again.";
pub const UNREADABLE: &str =
    "Composio answered in a form eve could not read. The call may have run: check before running it again.";

/// The token endpoint's errors that mean the refresh token or the client is gone for good (RFC 6749 5.2).
const REFUSED: [&str; 3] = ["invalid_grant", "invalid_client", "unauthorized_client"];
/// a toolkit slug: gmail, googlecalendar, google_maps
static SLUG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\A[a-z0-9][a-z0-9_]{0,63}\z").expect("SLUG"));
static HOST: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\A[a-z0-9-]+(?:\.[a-z0-9-]+)*\z").expect("HOST"));
static BLANK_RUNS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n{3,}").expect("BLANK_RUNS"));
static SHOWN_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\A[A-Za-z0-9_.-]{1,64}\z").expect("SHOWN_NAME"));
/// Where a COMPOSIO_MANAGE_CONNECTIONS answer may put a toolkit's sign-in link. Its own description names
/// redirect_url; Composio documents no shape beyond the envelope, so the others are read too, and link_ok decides.
const LINK_KEYS: [&str; 5] = ["redirect_url", "redirectUrl", "connect_link", "connectLink", "link"];
const ACTIVE: [&str; 2] = ["active", "connected"];
/// failed after the call was sent (httpx's ReadTimeout, ReadError, RemoteProtocolError)
const MAY_HAVE_RUN_KINDS: [&str; 3] = ["ReadTimeout", "ReadError", "RemoteProtocolError"];

// ---- pure: names, links, answers ------------------------------------------------------------------------------

/// `_clip(text, limit)`: `" ".join(str(text or "").split())`, clipped to `limit` characters with "…".
fn clip(text: &str, limit: usize) -> String {
    eve_apps_mcp::clip(text, limit)
}

/// `_text(value)`: the string, or "" for anything else.
fn text_of(value: Option<&Value>) -> &str {
    match value {
        Some(Value::String(s)) => s,
        _ => "",
    }
}

/// `_expiry(value)`: `expires_at` as epoch seconds; 0 (lapsed) for anything that is not a finite number.
fn expiry(value: Option<&Value>) -> f64 {
    match value {
        Some(Value::Number(n)) => n.as_f64().filter(|f| f.is_finite()).unwrap_or(0.0),
        _ => 0.0, // a bool is not a number here
    }
}

/// `_digest(token)`: SHA-256 of the token, in hex.
fn digest(token: &str) -> String {
    Sha256::digest(token.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// `_cooling(until, secs, now)`: whether a cool-off of `secs` seconds that ends at `until` still runs at `now`. One
/// that would end more than `secs` from now is over: the wall clock was set back, and the cool-off is not
/// stretched with it.
fn cooling(until: f64, secs: f64, now: f64) -> bool {
    now < until && until <= now + secs
}

/// Python's `value == 1` for a JSON value: 1, 1.0 and true.
fn eq_one(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Number(n)) => n.as_f64() == Some(1.0),
        Some(Value::Bool(b)) => *b,
        _ => false,
    }
}

/// `toolkit_slug(toolkit)`: `toolkit` as a Composio toolkit slug (lowercase letters, digits and underscores), or
/// None.
pub fn toolkit_slug(toolkit: &Value) -> Option<String> {
    match toolkit {
        Value::String(s) => toolkit_slug_str(s),
        _ => None,
    }
}

/// [`toolkit_slug`] of a string.
pub fn toolkit_slug_str(toolkit: &str) -> Option<String> {
    let text = strip(toolkit).to_lowercase();
    SLUG.is_match(&text).then_some(text)
}

/// `link_ok(url)` for any JSON value: only a string can be a link.
pub fn link_ok(url: &Value) -> bool {
    match url {
        Value::String(s) => link_ok_str(s),
        _ => false,
    }
}

/// Whether `url` may go to the browser as a Connect Link: `https` on composio.dev or a subdomain of it (Composio
/// documents `https://connect.composio.dev/link/ln_…`), on the default port, with no user name or password, in
/// printable ASCII with no space or backslash. A browser and Python then read the same host: a backslash, a user
/// part, a percent sign or a letter outside ASCII in the host is how a link that reads as Composio's goes
/// somewhere else.
///
/// The URL is split as Python's `urllib.parse.urlsplit` splits it (its scheme and its netloc, which is all this
/// reads). Where urlsplit raises ValueError (a bracket in the netloc that does not close or holds no IP), Python
/// refuses the link; here the bracket fails the host's pattern instead, so the answer is the same.
pub fn link_ok_str(url: &str) -> bool {
    let n = url.chars().count();
    let printable_ascii = url.bytes().all(|b| (0x20..=0x7e).contains(&b));
    if n == 0 || n > 4096 || !printable_ascii {
        return false;
    }
    if url.contains(' ') || url.contains('\\') {
        return false;
    }
    let (scheme, netloc) = split_scheme_netloc(url);
    if scheme != "https" || netloc.contains('@') {
        return false;
    }
    let lowered = netloc.to_ascii_lowercase();
    let (host, port) = match lowered.split_once(':') {
        Some((h, p)) => (h, p),
        None => (lowered.as_str(), ""),
    };
    if !(port.is_empty() || port == "443") || !HOST.is_match(host) {
        return false;
    }
    host == "composio.dev" || host.ends_with(".composio.dev")
}

/// `urlsplit(url)`'s scheme (lowercased) and netloc, for printable ASCII with no space.
fn split_scheme_netloc(url: &str) -> (String, &str) {
    const SCHEME_CHARS: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789+-.";
    let mut scheme = String::new();
    let mut rest = url;
    if let Some(i) = url.find(':') {
        let head = &url[..i];
        if i > 0 && url.as_bytes()[0].is_ascii_alphabetic() && head.chars().all(|c| SCHEME_CHARS.contains(c)) {
            scheme = head.to_ascii_lowercase();
            rest = &url[i + 1..];
        }
    }
    let Some(after) = rest.strip_prefix("//") else {
        return (scheme, "");
    };
    let delim = after.find(['/', '?', '#']).unwrap_or(after.len());
    (scheme, &after[..delim])
}

/// `describe(tool)`: Composio's description of a meta tool as the model reads it: whole, less CATALOG_LINE,
/// trailing spaces and runs of blank lines; the tool's title or name when it has none; clipped past DESC_MAX.
pub fn describe(tool: &Obj) -> String {
    let raw = text_of(tool.get("description"));
    let lines: Vec<&str> = splitlines(strip(raw))
        .into_iter()
        .filter(|line| !strip(line).starts_with(CATALOG_LINE))
        .map(rstrip)
        .collect();
    let joined = lines.join("\n");
    let mut text = strip(&BLANK_RUNS.replace_all(&joined, "\n\n")).to_string();
    if text.is_empty() {
        text = match tool.get("title") {
            Some(Value::String(t)) if !strip(t).is_empty() => strip(t).to_string(),
            _ => tool.get("name").filter(|v| truthy(v)).map(py_str).unwrap_or_default(),
        };
    }
    if text.chars().count() <= DESC_MAX {
        return text;
    }
    format!("{}…", rstrip(take_chars(&text, DESC_MAX - 1)))
}

/// `function_tool(tool)`: one meta tool as a Responses-API function: its own name, `describe`'s description, its
/// input schema slimmed (eve_apps_mcp::slim_schema). Not strict: Composio's schemas are not written for strict
/// mode, which would make every optional argument required.
pub fn function_tool(tool: &Obj) -> Value {
    let empty = json!({});
    let schema = tool.get("inputSchema").filter(|v| truthy(v)).unwrap_or(&empty);
    let mut params = eve_apps_mcp::slim_schema(schema);
    if params.get("type") != Some(&json!("object")) || !params.is_object() {
        params = json!({"type": "object", "properties": {}});
    }
    if let Value::Object(p) = &mut params {
        let props = p.entry("properties").or_insert_with(|| json!({}));
        if !props.is_object() {
            // a schema the model's API would refuse, whole turn and all
            params = json!({"type": "object", "properties": {}});
        }
    }
    json!({"type": "function", "name": tool.get("name").map(py_str).unwrap_or_default(),
           "description": describe(tool), "parameters": params, "strict": false})
}

/// `offered(listed)`: the meta tools in a tools/list answer, as the brain offers them, in META_TOOLS order. The
/// first of each name counts; any other tool is left out.
pub fn offered(listed: &[Value]) -> Vec<Value> {
    let mut found: IndexMap<&str, &Obj> = IndexMap::new();
    for tool in listed {
        if let Value::Object(t) = tool {
            if let Some(Value::String(name)) = t.get("name") {
                if let Some(meta) = META_TOOLS.iter().find(|m| **m == name.as_str()) {
                    found.entry(*meta).or_insert(t);
                }
            }
        }
    }
    META_TOOLS.iter().filter_map(|name| found.get(*name).map(|t| function_tool(t))).collect()
}

/// `json.loads(text)` when it is a JSON object; None for anything else (ValueError, RecursionError, not a dict).
fn loads_object(text: &str) -> Option<Obj> {
    match json_loads(text) {
        Ok(Value::Object(m)) => Some(m),
        _ => None,
    }
}

/// The text parts of a tools/call result's `content` list: `{"type": "text", "text": <str>}` each.
fn text_parts(result: &Obj) -> Vec<&str> {
    match result.get("content") {
        Some(Value::Array(content)) => content
            .iter()
            .filter_map(|c| match c {
                Value::Object(c) if c.get("type") == Some(&json!("text")) => c.get("text").and_then(Value::as_str),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// `_payload(result)`: Composio's own answer in a tools/call result: the structured content when it is an object,
/// else the text when it is one JSON object. None for anything else: the text is then read as it is.
pub fn payload(result: &Value) -> Option<Obj> {
    let Value::Object(result) = result else {
        return None;
    };
    if let Some(Value::Object(structured)) = result.get("structuredContent") {
        return Some(structured.clone());
    }
    let texts = text_parts(result);
    if texts.len() != 1 {
        return None;
    }
    loads_object(texts[0])
}

/// `_answer_object(result)`: Composio's own object, for Settings' bookkeeping (which apps are connected, a Connect
/// Link): [`payload`], or else the one text part that is a JSON object. Composio puts notes in parts of their own
/// after its answer (seen 2026-10-14: a custom-MCP hint and agent guidance after COMPOSIO_SEARCH_TOOLS's JSON),
/// and those must not hide it. None when no part, or more than one, is an object. The model's reading stays
/// `payload`'s: every part.
pub fn answer_object(result: &Value) -> Option<Obj> {
    let found = payload(result);
    let Value::Object(r) = result else {
        return found;
    };
    if found.is_some() || !matches!(r.get("content"), Some(Value::Array(_))) {
        return found;
    }
    let objects: Vec<Obj> = text_parts(r)
        .into_iter()
        .map(strip)
        .filter(|text| text.starts_with('{'))
        .filter_map(loads_object)
        .collect();
    if objects.len() == 1 {
        objects.into_iter().next()
    } else {
        None
    }
}

/// `result_for_model(result, payload)`: a tools/call result as the brain reads it, given its [`payload`]. When it
/// carries Composio's own object (`data`, `error`, `successful`), that object with `ok` added: the shape
/// eve_composio_tools::ComposioBridge returns, so the model reads Composio alike in both eves, and the JSON is not
/// sent back to it as one escaped string. Otherwise apps_mcp's reading (the text). Longer than
/// eve_apps_mcp::RESULT_CLIP, it is cut as a direct app's is.
pub fn result_for_model(result: &Value, payload: Option<&Obj>) -> Obj {
    let Value::Object(result) = result else {
        let mut out = Obj::new();
        out.insert("ok".into(), json!(false));
        out.insert("reason".into(), json!("Composio sent no result"));
        return out;
    };
    let Some(payload) = payload else {
        let mut plain = result.clone();
        let content = match result.get("content") {
            Some(c @ Value::Array(_)) => c.clone(),
            _ => json!([]),
        };
        plain.insert("content".into(), content);
        return eve_apps_mcp::result_for_model(&plain);
    };
    let error = payload.get("error");
    let ok = !result.get("isError").is_some_and(truthy)
        && payload.get("successful") != Some(&Value::Bool(false))
        && payload.get("ok") != Some(&Value::Bool(false))
        && !error.is_some_and(truthy);
    let mut out = Obj::new();
    out.insert("ok".into(), json!(ok));
    for (k, v) in payload {
        if k != "ok" {
            out.insert(k.clone(), v.clone());
        }
    }
    if !ok && !out.contains_key("reason") {
        let why = match error {
            Some(Value::String(s)) => s.clone(),
            Some(e) if truthy(e) => dumps(e, true),
            _ => String::new(),
        };
        let why = if why.is_empty() { "Composio did not finish the call".to_string() } else { why };
        out.insert("reason".into(), json!(clip(&why, 300)));
    }
    let raw = dumps(&Value::Object(out.clone()), false);
    if raw.chars().count() <= eve_apps_mcp::RESULT_CLIP {
        return out;
    }
    let mut clipped = Obj::new();
    clipped.insert("ok".into(), json!(ok));
    clipped.insert("result".into(), json!(format!("{}…", take_chars(&raw, eve_apps_mcp::RESULT_CLIP))));
    clipped.insert("clipped".into(), json!(true));
    if let Some(reason) = out.get("reason") {
        clipped.insert("reason".into(), reason.clone());
    }
    clipped
}

/// `_data(payload)`: the answer's `data` when it is an object, else the answer itself.
fn data_of(payload: &Obj) -> &Obj {
    match payload.get("data") {
        Some(Value::Object(d)) => d,
        _ => payload,
    }
}

/// `search_statuses(payload)`: {toolkit: connected} from a COMPOSIO_SEARCH_TOOLS answer:
/// `data.toolkit_connection_statuses`, each item's `toolkit` and `has_active_connection` (Composio's docs,
/// 2026-10-13). An item that does not read is skipped.
pub fn search_statuses(payload: &Value) -> IndexMap<String, bool> {
    let mut out = IndexMap::new();
    let Value::Object(payload) = payload else {
        return out;
    };
    let items: Vec<Value> = match data_of(payload).get("toolkit_connection_statuses") {
        // keyed by toolkit, should a version send it that way
        Some(Value::Object(keyed)) => keyed
            .iter()
            .filter_map(|(key, value)| match value {
                Value::Object(v) => {
                    let mut item = Obj::new();
                    item.insert("toolkit".into(), json!(key));
                    for (k, x) in v {
                        item.insert(k.clone(), x.clone());
                    }
                    Some(Value::Object(item))
                }
                _ => None,
            })
            .collect(),
        Some(Value::Array(list)) => list.clone(),
        _ => Vec::new(),
    };
    for item in &items {
        if let Value::Object(item) = item {
            let slug = item.get("toolkit").and_then(toolkit_slug);
            if let (Some(slug), Some(Value::Bool(active))) = (slug, item.get("has_active_connection")) {
                out.insert(slug, *active);
            }
        }
    }
    out
}

/// `a or b or c` over JSON values: the first truthy one, else the last.
fn first_truthy<'a>(values: &[Option<&'a Value>]) -> Option<&'a Value> {
    for v in values {
        if v.is_some_and(truthy) {
            return *v;
        }
    }
    values.last().copied().flatten()
}

/// `_manage_items(payload)`: each toolkit's part of a COMPOSIO_MANAGE_CONNECTIONS answer, by slug: `data.results`
/// keyed by toolkit (the documented envelope), or a list of items that each name theirs.
pub fn manage_items(payload: &Value) -> IndexMap<String, Obj> {
    let mut out: IndexMap<String, Obj> = IndexMap::new();
    let Value::Object(payload) = payload else {
        return out;
    };
    match data_of(payload).get("results") {
        Some(Value::Object(results)) => {
            for (key, item) in results {
                if let (Some(slug), Value::Object(item)) = (toolkit_slug_str(key), item) {
                    out.entry(slug).or_insert_with(|| item.clone());
                }
            }
        }
        Some(Value::Array(results)) => {
            for item in results {
                if let Value::Object(item) = item {
                    let named = first_truthy(&[item.get("toolkit"), item.get("toolkit_slug"), item.get("slug")]);
                    if let Some(slug) = named.and_then(toolkit_slug) {
                        out.entry(slug).or_insert_with(|| item.clone());
                    }
                }
            }
        }
        _ => {}
    }
    out
}

/// `_item_link(item)`: the first of LINK_KEYS that holds a string with something in it, stripped.
fn item_link(item: &Obj) -> Option<String> {
    LINK_KEYS.iter().find_map(|key| match item.get(*key) {
        Some(Value::String(s)) if !strip(s).is_empty() => Some(strip(s).to_string()),
        _ => None,
    })
}

/// `_item_active(item)`: true when a toolkit's part says its connection is active, false when it says it is not
/// (or brings a link to make one), None when it says neither.
pub fn item_active(item: &Obj) -> Option<bool> {
    for key in ["has_active_connection", "is_active", "connected"] {
        if let Some(Value::Bool(b)) = item.get(key) {
            return Some(*b);
        }
    }
    if let Some(Value::String(status)) = first_truthy(&[item.get("status"), item.get("connection_status")]) {
        if !strip(status).is_empty() {
            return Some(ACTIVE.contains(&strip(status).to_lowercase().as_str()));
        }
    }
    if item_link(item).is_some() {
        Some(false)
    } else {
        None
    }
}

/// `_redirect_urls(value)`: each `redirect_url` in `value`, a few levels deep at most.
fn redirect_urls(value: &Value, depth: usize, out: &mut Vec<String>) {
    if depth > 6 {
        return;
    }
    let values: Vec<&Value> = match value {
        Value::Object(m) => {
            if let Some(Value::String(found)) = m.get("redirect_url") {
                if !strip(found).is_empty() {
                    out.push(strip(found).to_string());
                }
            }
            m.values().collect()
        }
        Value::Array(items) => items.iter().collect(),
        _ => return,
    };
    for item in values {
        if matches!(item, Value::Object(_) | Value::Array(_)) {
            redirect_urls(item, depth + 1, out);
        }
    }
}

/// `_only_link(value)`: the `redirect_url` in `value`, at any depth, when there is one; None for none, and for two
/// that differ: nothing then says which app each is for.
fn only_link(value: &Value) -> Option<String> {
    let mut found = Vec::new();
    redirect_urls(value, 0, &mut found);
    let distinct: BTreeSet<&String> = found.iter().collect();
    if distinct.len() == 1 {
        found.into_iter().next()
    } else {
        None
    }
}

/// `connect_answer(payload, slug)`: (link, connected) for the one toolkit a COMPOSIO_MANAGE_CONNECTIONS call asked
/// about. `connected` is None when the answer does not say; the link is not checked here (link_ok). Only that
/// toolkit's part speaks for it: an answer whose parts are all other apps' says nothing of it, and an answer with
/// no parts gives a link only when it holds a single one.
pub fn connect_answer(payload: Option<&Obj>, slug: &str) -> (Option<String>, Option<bool>) {
    let Some(payload) = payload else {
        return (None, None);
    };
    let whole = Value::Object(payload.clone());
    let items = manage_items(&whole);
    let mut item = items.get(slug);
    if item.is_none() && items.len() == 1 {
        // one toolkit was asked about: its part, whatever its key
        item = items.values().next();
    }
    if item.is_none() && !items.is_empty() {
        // other apps' parts only: no link and no status of this one
        return (None, None);
    }
    let mut active = item.and_then(item_active);
    if active.is_none() {
        if let Some(Value::Object(summary)) = data_of(payload).get("summary") {
            if eq_one(summary.get("total_toolkits")) {
                active = if eq_one(summary.get("active_connections")) {
                    Some(true)
                } else if eq_one(summary.get("initiated_connections")) {
                    Some(false)
                } else {
                    None
                };
            }
        }
    }
    let link = match item {
        Some(item) => item_link(item).or_else(|| only_link(&Value::Object(item.clone()))),
        None => only_link(&whole),
    };
    (link, active)
}

// ---- HTTP -----------------------------------------------------------------------------------------------------

/// The daemon's transport to Composio: reqwest with httpx's `Timeout(read_secs, connect=CONNECT_TIMEOUT_SECS)`,
/// following no redirects (httpx's default). A failure carries httpx's class name: a request that failed after
/// the connection was made, other than a timeout, reads as RemoteProtocolError (the call may have run).
pub struct ConnectTransport {
    client: reqwest::Client,
}

impl ConnectTransport {
    pub fn new(read_secs: f64, connect_secs: f64) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs_f64(connect_secs.max(0.0)))
            .read_timeout(Duration::from_secs_f64(read_secs.max(0.0)))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { client }
    }
}

fn reqwest_error(e: &reqwest::Error) -> TransportError {
    let kind = if e.is_timeout() && e.is_connect() {
        "ConnectTimeout"
    } else if e.is_timeout() {
        "ReadTimeout"
    } else if e.is_connect() {
        "ConnectError"
    } else if e.is_body() || e.is_decode() {
        "ReadError"
    } else if e.is_builder() {
        "UnsupportedProtocol"
    } else if e.is_request() {
        "RemoteProtocolError"
    } else {
        "RequestError"
    };
    TransportError::new(kind, e.to_string())
}

#[async_trait]
impl Transport for ConnectTransport {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let method = reqwest::Method::from_bytes(request.method.as_bytes())
            .map_err(|e| TransportError::new("UnsupportedProtocol", e.to_string()))?;
        let mut b = self.client.request(method, &request.url);
        for (k, v) in &request.headers {
            b = b.header(k, v);
        }
        b = b.body(request.body.unwrap_or_default());
        let resp = b.send().await.map_err(|e| reqwest_error(&e))?;
        let status = resp.status().as_u16();
        let headers = resp
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned()))
            .collect();
        let body = resp.bytes_stream().map(|r| r.map_err(|e| reqwest_error(&e)));
        Ok(HttpResponse { status, headers, body: Box::pin(body) })
    }
}

/// A response body read whole.
async fn read_body(response: HttpResponse) -> Result<(u16, Vec<(String, String)>, Vec<u8>), TransportError> {
    let HttpResponse { status, headers, mut body } = response;
    let mut content = Vec::new();
    while let Some(chunk) = body.next().await {
        let chunk: Bytes = chunk?;
        content.extend_from_slice(&chunk);
    }
    Ok((status, headers, content))
}

/// Python's `type(e).__name__` for an OSError.
fn os_error_name(e: &std::io::Error) -> &'static str {
    use std::io::ErrorKind::*;
    match e.kind() {
        PermissionDenied => "PermissionError",
        NotFound => "FileNotFoundError",
        AlreadyExists => "FileExistsError",
        IsADirectory => "IsADirectoryError",
        NotADirectory => "NotADirectoryError",
        Interrupted => "InterruptedError",
        TimedOut => "TimeoutError",
        _ => "OSError",
    }
}

/// `time.time()`.
pub fn wall_clock() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// `time.monotonic()`: seconds since a fixed point in this process.
pub fn monotonic_clock() -> f64 {
    static ANCHOR: LazyLock<Instant> = LazyLock::new(Instant::now);
    ANCHOR.elapsed().as_secs_f64()
}

// ---- the sign-in ----------------------------------------------------------------------------------------------

/// What a bridge needs of its token source (Python's duck type: a callable, with `refresh`, `hold` and
/// `needs_sign_in` when it has them). A plain closure is one: it has no refresh (the bridge then gives up after
/// one 401), no hold, and never needs a new sign-in.
#[async_trait]
pub trait ConnectTokens: Send + Sync {
    /// `token_source()`: the access token to send, or None.
    async fn token(&self) -> Option<String>;
    /// `refresh(rejected)`: after Composio refused `rejected` (HTTP 401), a new access token, or None. A source
    /// with no refresh answers None.
    async fn refresh(&self, _rejected: Option<&str>) -> Option<String> {
        None
    }
    /// `hold(rejected)`: Composio refused `rejected`, the token `refresh` just handed out, too.
    async fn hold(&self, _rejected: Option<&str>) {}
    /// `needs_sign_in`: only a new sign-in helps.
    fn needs_sign_in(&self) -> bool {
        false
    }
}

#[async_trait]
impl<F> ConnectTokens for F
where
    F: Fn() -> Option<String> + Send + Sync,
{
    async fn token(&self) -> Option<String> {
        self()
    }
}

#[derive(Default)]
struct TokenState {
    /// SHA-256 of a refresh token the token endpoint refused: never sent again
    refused: String,
    /// SHA-256 of an access token Composio answered 401 to: never sent again
    rejected: String,
    /// no refresh before this, after one that failed for a reason that may pass
    retry_at: f64,
    /// (SHA-256 of the token hold() set aside, the hold's end)
    held: Option<(String, f64)>,
    /// (refresh token it replaced, a sign-in not saved)
    unsaved: Option<(String, Obj)>,
}

/// The Composio Connect sign-in in `path` (composio-connect.json, written by onboarding's ComposioConnectLogin).
/// [`get`](Self::get) gives the access token, refreshed first when it has under REFRESH_EARLY_SECS left, or None
/// when there is no usable sign-in. `refresh(rejected)` is the one refresh after Composio answers 401, and
/// `hold(rejected)` follows a 401 to the token that refresh handed out. Nothing here fails past a log line: a turn
/// goes on without the apps.
pub struct ConnectTokenSource {
    pub path: PathBuf,
    http: Arc<dyn Transport>,
    clock: Clock,
    write: Writer,
    /// Python's threading.Lock: one get, refresh or hold at a time
    op: tokio::sync::Mutex<()>,
    st: Mutex<TokenState>,
}

impl std::fmt::Debug for ConnectTokenSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        write!(f, "ConnectTokenSource({name})")
    }
}

impl Default for ConnectTokenSource {
    fn default() -> Self {
        Self::new(None)
    }
}

impl ConnectTokenSource {
    /// `ConnectTokenSource(path)`: None is CONNECT_TOKEN_FILE. The token endpoint is reached over reqwest with a
    /// TOKEN_TIMEOUT_SECS timeout, and the clock is `time.time`.
    pub fn new(path: Option<&Path>) -> Self {
        let path = match path {
            Some(p) => expanduser(p),
            None => expanduser(Path::new(CONNECT_TOKEN_FILE)),
        };
        Self {
            path,
            http: Arc::new(eve_google_rest::ReqwestTransport::new(TOKEN_TIMEOUT_SECS)),
            clock: Arc::new(wall_clock),
            write: Arc::new(|path: &Path, data: &Obj| write_private_json(path, data)),
            op: tokio::sync::Mutex::new(()),
            st: Mutex::new(TokenState::default()),
        }
    }

    /// The `transport=` argument: the token endpoint over this transport.
    pub fn with_transport(mut self, transport: Arc<dyn Transport>) -> Self {
        self.http = transport;
        self
    }

    /// The `clock=` argument (wall-clock seconds).
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// How a refreshed sign-in is written (write_private_json; a seam for a disk that is full).
    pub fn with_writer(mut self, write: Writer) -> Self {
        self.write = write;
        self
    }

    fn now(&self) -> f64 {
        (self.clock)()
    }

    /// `__call__`: the access token to send, or None.
    pub async fn get(&self) -> Option<String> {
        let _turn = self.op.lock().await;
        self.current(None).await
    }

    /// `refresh(rejected)`: after Composio refused `rejected` (HTTP 401): a new access token, or None. When the
    /// file already holds another token that has not lapsed (another call refreshed it meanwhile), that one, with
    /// no request. None while the sign-in is held (`hold`).
    pub async fn refresh(&self, rejected: Option<&str>) -> Option<String> {
        let _turn = self.op.lock().await;
        self.current(Some(rejected.unwrap_or(""))).await
    }

    /// `hold(rejected)`: Composio answered 401 to `rejected` too, the token `refresh` had just handed out. That may
    /// pass (S1: /mcp's 401 also covers Composio not finding the account), so for REFUSED_RETRY_SECS no token goes
    /// out and no refresh is asked for: a fresh pair would be refused as well. Then the same token is tried again,
    /// and after a 401 refreshed once more. A file that no longer holds `rejected` (the setup page signed in again
    /// meanwhile) is left alone: that sign-in gets its own try. A hold never asks for a new sign-in
    /// (needs_sign_in): that is for a sign-in that cannot be renewed.
    pub async fn hold(&self, rejected: Option<&str>) {
        {
            let _turn = self.op.lock().await;
            let data = self.read();
            let access = text_of(data.get("access_token"));
            let Some(rejected) = rejected.filter(|r| !r.is_empty()) else {
                return;
            };
            if access != rejected {
                return;
            }
            let until = self.now() + REFUSED_RETRY_SECS;
            self.st.lock().held = Some((digest(access), until));
        }
        tracing::warn!(
            "composio connect: Composio refused a token fresh from a refresh; trying the sign-in again in {} s",
            REFUSED_RETRY_SECS as i64
        );
    }

    /// `needs_sign_in`: only a new sign-in helps: the token endpoint refused the file's refresh token, or the file
    /// holds neither an access token that has not lapsed (and that Composio did not refuse) nor a refresh token
    /// (or there is no file). A sign-in held after a 401 (`hold`) is not one of these: that may pass.
    pub fn needs_sign_in(&self) -> bool {
        let data = self.read();
        let token = text_of(data.get("refresh_token"));
        let (refused, rejected) = {
            let st = self.st.lock();
            (st.refused.clone(), st.rejected.clone())
        };
        if !token.is_empty() && digest(token) == refused {
            return true;
        }
        let access = text_of(data.get("access_token"));
        let live = !access.is_empty() && digest(access) != rejected && expiry(data.get("expires_at")) > self.now();
        !(live || (!token.is_empty() && !text_of(data.get("client_id")).is_empty()))
    }

    /// `_read()`: the file's sign-in, or {} when it holds none eve can use (missing, unreadable, another version or
    /// issuer). A refreshed sign-in that could not be saved stands in for the one it replaced.
    fn read(&self) -> Obj {
        let mut data = eve_apps_mcp::read_json(&self.path);
        {
            let mut st = self.st.lock();
            if let Some((used, saved)) = &st.unsaved {
                if text_of(data.get("refresh_token")) == used {
                    data = saved.clone();
                } else {
                    st.unsaved = None; // the file moved on: a new sign-in, or a later save worked
                }
            }
        }
        let version_ok = match data.get("version") {
            Some(Value::Number(n)) => n.as_f64() == Some(TOKEN_FILE_VERSION as f64),
            Some(Value::Bool(b)) => *b && TOKEN_FILE_VERSION == 1,
            _ => false,
        };
        let issuer_ok = match data.get("issuer") {
            None => true,
            Some(Value::String(s)) => s == CONNECT_ISSUER,
            Some(_) => false,
        };
        if !version_ok || !issuer_ok {
            return Obj::new();
        }
        data
    }

    /// `_current(rejected)`: the token to send. `rejected` is None for an ordinary call, else the access token
    /// Composio just answered 401 to ("" for none): it is never sent again, and another one that has not lapsed (a
    /// refresh by another call meanwhile) is used as it is. None, with no refresh, while the file's token is held
    /// (`hold`).
    async fn current(&self, rejected: Option<&str>) -> Option<String> {
        let data = self.read();
        if data.is_empty() {
            return None;
        }
        let mut access = text_of(data.get("access_token")).to_string();
        let now = self.now();
        if self.holding(&access, now) {
            return None;
        }
        {
            let mut st = self.st.lock();
            if rejected.is_some_and(|r| !r.is_empty() && access == r) {
                st.rejected = digest(&access);
            }
            if !access.is_empty() && digest(&access) == st.rejected {
                access.clear();
            }
        }
        let expires = expiry(data.get("expires_at"));
        let early = if rejected.is_none() { REFRESH_EARLY_SECS } else { 0.0 };
        if !access.is_empty() && expires > now + early {
            return Some(access);
        }
        // Refresh. When that does not work, a token that has not lapsed still goes (an early refresh failed).
        match self.refresh_with(&data).await {
            Some(fresh) => Some(fresh),
            None => (!access.is_empty() && expires > now).then_some(access),
        }
    }

    /// `_holding(access, now)`: whether `access` is the token hold() set aside and its hold still runs. A hold that
    /// is over, or whose token the file no longer holds (a new sign-in), is dropped.
    fn holding(&self, access: &str, now: f64) -> bool {
        let mut st = self.st.lock();
        let Some((held, until)) = st.held.clone() else {
            return false;
        };
        if !access.is_empty() && digest(access) == held && cooling(until, REFUSED_RETRY_SECS, now) {
            return true;
        }
        st.held = None;
        false
    }

    /// `_refresh(data)`: a new access token for the file's refresh token, saved with what came with it. None when
    /// there is no refresh token, Composio refused this one before, a failed refresh is still cooling off, or this
    /// one fails.
    async fn refresh_with(&self, data: &Obj) -> Option<String> {
        let token = text_of(data.get("refresh_token")).to_string();
        let client_id = text_of(data.get("client_id")).to_string();
        {
            let st = self.st.lock();
            if token.is_empty()
                || client_id.is_empty()
                || digest(&token) == st.refused
                || cooling(st.retry_at, REFRESH_RETRY_SECS, self.now())
            {
                return None;
            }
        }
        let form: [(&str, String); 4] = [
            ("grant_type", "refresh_token".into()),
            ("refresh_token", token.clone()),
            ("client_id", client_id),
            ("resource", CONNECT_RESOURCE.into()),
        ];
        let request = HttpRequest {
            method: "POST".into(),
            url: CONNECT_TOKEN_URL.into(),
            headers: vec![
                ("Accept".into(), "application/json".into()),
                ("Content-Type".into(), "application/x-www-form-urlencoded".into()),
            ],
            body: Some(urlencode(&form).into_bytes()),
        };
        let sent = match self.http.send(request).await {
            Ok(response) => read_body(response).await,
            Err(e) => Err(e),
        };
        let (status, headers, body) = match sent {
            Ok(read) => read,
            Err(e) => return self.failed(&e.kind),
        };
        let answer = token_answer(&eve_brain::seams::HttpResponse { status, headers, body });
        if status != 200 {
            let word = oauth_word(&answer);
            let word_or = if word.is_empty() { "no error code" } else { word.as_str() };
            if status == 401 || REFUSED.contains(&word.as_str()) {
                self.st.lock().refused = digest(&token);
                tracing::warn!(
                    "composio connect: Composio refused the sign-in (HTTP {status}, {word_or}); the owner signs in to \
                     Composio again in Settings → Apps"
                );
                return None;
            }
            return self.failed(&format!("HTTP {status}, {word_or}"));
        }
        let access = match answer.get("access_token") {
            Some(Value::String(a)) if TOKEN_SHAPE.is_match(a) => a.clone(),
            _ => return self.failed("no usable access token in the answer"),
        };
        let rotated = match answer.get("refresh_token") {
            Some(Value::String(r)) if TOKEN_SHAPE.is_match(r) => r.clone(),
            _ => token.clone(),
        };
        self.st.lock().retry_at = 0.0;
        self.save(&token, &access, &rotated, &answer);
        tracing::debug!("composio connect: refreshed the sign-in");
        Some(access)
    }

    /// `_failed(why)`: a refresh that failed for a reason that may pass; none for REFRESH_RETRY_SECS.
    fn failed(&self, why: &str) -> Option<String> {
        self.st.lock().retry_at = self.now() + REFRESH_RETRY_SECS;
        tracing::warn!(
            "composio connect: the sign-in was not refreshed ({why}); trying again in {} s",
            REFRESH_RETRY_SECS as i64
        );
        None
    }

    /// `_save(used, access, rotated, answer)`: write the refreshed sign-in back over the one it came from. A new
    /// sign-in written by the setup page while the refresh ran is kept instead; a refresh that cannot be written is
    /// held in memory (`_unsaved`).
    fn save(&self, used: &str, access: &str, rotated: &str, answer: &Obj) {
        let mut data = self.read();
        if text_of(data.get("refresh_token")) != used {
            tracing::info!("composio connect: a new sign-in arrived during a refresh; it is kept");
            return;
        }
        data.insert("access_token".into(), json!(access));
        data.insert("refresh_token".into(), json!(rotated));
        data.insert("expires_at".into(), json!(token_expires_at(answer, access, self.now())));
        if let Some(Value::String(scope)) = answer.get("scope") {
            if !strip(scope).is_empty() {
                data.insert("scope".into(), json!(scope));
            }
        }
        match (self.write)(&self.path, &data) {
            Ok(()) => self.st.lock().unsaved = None,
            Err(e) => {
                let kind = os_error_name(&e);
                self.st.lock().unsaved = Some((used.to_string(), data));
                tracing::warn!(
                    "composio connect: the refreshed sign-in was not saved ({kind}); it is kept until a restart"
                );
            }
        }
    }
}

#[async_trait]
impl ConnectTokens for ConnectTokenSource {
    async fn token(&self) -> Option<String> {
        self.get().await
    }

    async fn refresh(&self, rejected: Option<&str>) -> Option<String> {
        ConnectTokenSource::refresh(self, rejected).await
    }

    async fn hold(&self, rejected: Option<&str>) {
        ConnectTokenSource::hold(self, rejected).await
    }

    fn needs_sign_in(&self) -> bool {
        ConnectTokenSource::needs_sign_in(self)
    }
}

// ---- the bridge -----------------------------------------------------------------------------------------------

/// Why a call was not made, or failed: `_SignedOut` (no sign-in Composio takes: no token to send, none or one
/// held, no refresh to be had, or a 401 to the token fresh from one), or the call's own error.
#[derive(Debug)]
enum Fail {
    SignedOut,
    Call(CallError),
}

/// What one lane last saw: the status of the last response (Python's `_saw` event hook) and the token its last
/// request sent (`_token`). Python keeps both per thread; a lane runs one call at a time (its `turn`), which is
/// the same thing.
#[derive(Default)]
struct Seen {
    status: Mutex<Option<u16>>,
    token: Mutex<Option<String>>,
}

/// A transport that notes each response's status.
struct SeeingTransport {
    inner: Arc<dyn Transport>,
    seen: Arc<Seen>,
}

#[async_trait]
impl Transport for SeeingTransport {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let response = self.inner.send(request).await?;
        *self.seen.status.lock() = Some(response.status);
        Ok(response)
    }
}

/// The token McpClient sends, noted as it goes.
struct SeeingToken {
    tokens: Arc<dyn ConnectTokens>,
    seen: Arc<Seen>,
}

#[async_trait]
impl eve_google_rest::TokenSource for SeeingToken {
    async fn token(&self) -> Option<String> {
        let token = self.tokens.token().await;
        *self.seen.token.lock() = token.clone();
        token
    }
}

/// One MCP session (McpClient makes one call at a time), with what its calls saw.
struct Lane {
    client: McpClient,
    seen: Arc<Seen>,
    tokens: SeeingToken,
    turn: tokio::sync::Mutex<()>,
}

impl Lane {
    fn new(url: &str, tokens: &Arc<dyn ConnectTokens>, transport: Arc<dyn Transport>) -> Self {
        let seen = Arc::new(Seen::default());
        let http: Arc<dyn Transport> = Arc::new(SeeingTransport { inner: transport, seen: seen.clone() });
        let client = McpClient::with_transport(
            url,
            SeeingToken { tokens: tokens.clone(), seen: seen.clone() },
            &[],
            http,
            "",
        );
        Self {
            client,
            tokens: SeeingToken { tokens: tokens.clone(), seen: seen.clone() },
            seen,
            turn: tokio::sync::Mutex::new(()),
        }
    }
}

/// What a lane is asked to do.
enum Op<'a> {
    List,
    Call(&'a str, &'a Obj),
}

/// The readers `execute` runs over a call's answer (Python's module functions, which its tests patch): a reader
/// that panics is caught as Python catches an exception there.
#[derive(Clone, Copy)]
pub struct Readers {
    pub result_for_model: fn(&Value, Option<&Obj>) -> Obj,
    pub search_statuses: fn(&Value) -> IndexMap<String, bool>,
}

impl Default for Readers {
    fn default() -> Self {
        Self { result_for_model, search_statuses }
    }
}

/// How a [`ConnectBridge`] is made: Python's keyword arguments.
#[derive(Clone)]
pub struct BridgeOptions {
    pub url: String,
    /// replaces the network for all three sessions (tests); None is reqwest with the timeouts above
    pub transport: Option<Arc<dyn Transport>>,
    pub backoff: Vec<f64>,
    /// `time.monotonic`
    pub clock: Clock,
    pub readers: Readers,
}

impl Default for BridgeOptions {
    fn default() -> Self {
        Self {
            url: MCP_URL.to_string(),
            transport: None,
            backoff: START_BACKOFF.to_vec(),
            clock: Arc::new(monotonic_clock),
            readers: Readers::default(),
        }
    }
}

#[derive(Default)]
struct Up {
    tools: Vec<Obj>,
    names: BTreeSet<String>,
}

struct Looks {
    /// toolkit -> connected, as last read or seen
    known: IndexMap<String, bool>,
    /// toolkit -> when that was read
    looked: IndexMap<String, f64>,
    look_retry_at: f64,
}

/// The person's apps through Composio Connect, wrapped so nothing in here fails into the chat loop. The token
/// source gives the access token ([`ConnectTokenSource`]); its `refresh(rejected)` is called once after a 401, and
/// its `hold(rejected)` after a 401 to the refreshed token. `BridgeOptions::transport` replaces the network
/// (tests).
pub struct ConnectBridge {
    tokens: Arc<dyn ConnectTokens>,
    backoff: Vec<f64>,
    clock: Clock,
    readers: Mutex<Readers>,
    // One MCP session each, so a Connect Link never waits behind the brain's three-minute workbench cell, nor
    // behind Settings reading which apps are connected.
    brain: Lane,
    linker: Lane,
    looker: Lane,
    up: Mutex<Up>,
    looks: Mutex<Looks>,
    /// one read of the connected apps at a time
    looking: tokio::sync::Mutex<()>,
    stop: tokio::sync::watch::Sender<bool>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    starts: AtomicUsize,
    closed: AtomicBool,
    last_failure: Mutex<String>,
}

impl std::fmt::Debug for ConnectBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = self.up.lock().names.len();
        write!(f, "ConnectBridge({}, {n} tools)", if n > 0 { "up" } else { "not up" })
    }
}

impl ConnectBridge {
    /// `ConnectBridge(token_source)` with the defaults: Connect's MCP URL over reqwest, START_BACKOFF.
    pub fn new(tokens: Arc<dyn ConnectTokens>) -> Self {
        Self::with_options(tokens, BridgeOptions::default())
    }

    /// `ConnectBridge(token_source, url=, transport=, backoff=, clock=)`.
    pub fn with_options(tokens: Arc<dyn ConnectTokens>, options: BridgeOptions) -> Self {
        let BridgeOptions { url, transport, backoff, clock, readers } = options;
        let backoff = if backoff.is_empty() { START_BACKOFF.to_vec() } else { backoff };
        let lane = |read_secs: f64| {
            let http: Arc<dyn Transport> = match &transport {
                Some(t) => t.clone(),
                None => Arc::new(ConnectTransport::new(read_secs, CONNECT_TIMEOUT_SECS)),
            };
            Lane::new(&url, &tokens, http)
        };
        let (brain, linker, looker) = (lane(CALL_TIMEOUT_SECS), lane(OWN_TIMEOUT_SECS), lane(OWN_TIMEOUT_SECS));
        Self {
            tokens,
            backoff,
            clock,
            readers: Mutex::new(readers),
            brain,
            linker,
            looker,
            up: Mutex::new(Up::default()),
            looks: Mutex::new(Looks {
                known: IndexMap::new(),
                looked: IndexMap::new(),
                look_retry_at: f64::NEG_INFINITY,
            }),
            looking: tokio::sync::Mutex::new(()),
            stop: tokio::sync::watch::channel(false).0,
            task: Mutex::new(None),
            starts: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
            last_failure: Mutex::new(String::new()),
        }
    }

    fn now(&self) -> f64 {
        (self.clock)()
    }

    /// Replace the readers `execute` and `connections` run over an answer (Python's tests patch the module's).
    pub fn set_readers(&self, readers: Readers) {
        *self.readers.lock() = readers;
    }

    // -- the duck type the brain reads (eve_composio_tools::ComposioBridge) --

    pub fn started(&self) -> bool {
        !self.up.lock().names.is_empty()
    }

    pub fn names(&self) -> BTreeSet<String> {
        self.up.lock().names.clone()
    }

    /// The meta tools as the brain offers them; each caller gets its own copy.
    pub fn tools(&self) -> Vec<Obj> {
        self.up.lock().tools.clone()
    }

    /// Always "": Connect holds one person's own accounts, with no personal and other accounts to tell apart (the
    /// split full eve's accounts_note is for).
    pub fn accounts_note(&self) -> String {
        String::new()
    }

    /// Only a new sign-in helps (ConnectTokenSource::needs_sign_in), for the status page.
    pub fn needs_sign_in(&self) -> bool {
        self.tokens.needs_sign_in()
    }

    /// `_why()`: what the model reads for a call made without a sign-in Composio takes: sign in again when only
    /// that helps (needs_sign_in, so it says what Settings shows); else try again later (a refresh that failed for
    /// a reason that may pass, a sign-in held after Composio refused it fresh, or a new sign-in that arrived
    /// meanwhile).
    fn why(&self) -> &'static str {
        if self.needs_sign_in() {
            SIGNED_OUT
        } else {
            NOT_RENEWED
        }
    }

    // -- starting --

    /// List the tools on a task, trying again with backoff (START_BACKOFF, its last step repeating) until it works
    /// or `close()`. Returns at once; a call while that task runs, or once the tools are up, does nothing. Needs a
    /// tokio runtime.
    pub fn start(self: &Arc<Self>) {
        let mut task = self.task.lock();
        if self.started()
            || self.closed.load(Ordering::SeqCst)
            || task.as_ref().is_some_and(|t| !t.is_finished())
        {
            return;
        }
        self.starts.fetch_add(1, Ordering::SeqCst);
        let me = self.clone();
        *task = Some(tokio::spawn(async move { me.keep_starting().await }));
    }

    /// How many start tasks this bridge has spawned (Python's `_thread` identity, for tests).
    pub fn start_runs(&self) -> usize {
        self.starts.load(Ordering::SeqCst)
    }

    /// Whether a start task is running.
    pub fn starting(&self) -> bool {
        self.task.lock().as_ref().is_some_and(|t| !t.is_finished())
    }

    async fn keep_starting(&self) {
        let mut stop = self.stop.subscribe();
        for attempt in 0usize.. {
            if *stop.borrow() || self.try_start().await {
                return;
            }
            let secs = self.backoff[attempt.min(self.backoff.len() - 1)];
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs_f64(secs.max(0.0))) => {}
                _ = stop.wait_for(|stopped| *stopped) => return,
            }
        }
    }

    /// One attempt to list Composio's tools; true once the meta tools are up. Never fails.
    pub async fn try_start(&self) -> bool {
        if self.started() {
            return true;
        }
        let listed = match self.authorized(&self.brain, Op::List).await {
            Ok(Value::Array(listed)) => listed,
            Ok(_) => Vec::new(),
            Err(Fail::SignedOut) => {
                let why = if self.needs_sign_in() { "signed out" } else { "the sign-in was not renewed" };
                self.failed(why);
                return false;
            }
            Err(Fail::Call(e)) => {
                self.failed(&e.kind);
                return false;
            }
        };
        let tools: Vec<Obj> = offered(&listed)
            .into_iter()
            .filter_map(|t| match t {
                Value::Object(m) => Some(m),
                _ => None,
            })
            .collect();
        if tools.is_empty() {
            self.failed(&format!("none of the meta tools among {} listed", listed.len()));
            return false;
        }
        let names: BTreeSet<String> = tools.iter().map(|t| text_of(t.get("name")).to_string()).collect();
        let others: Vec<String> = listed
            .iter()
            .filter_map(|t| t.as_object().map(|t| t.get("name").map(py_str).unwrap_or_else(|| "None".into())))
            .filter(|n| !names.contains(n))
            .collect();
        let shown: Vec<&str> =
            others.iter().filter(|n| SHOWN_NAME.is_match(n)).take(10).map(String::as_str).collect();
        let count = tools.len();
        {
            let mut up = self.up.lock();
            up.tools = tools;
            up.names = names;
        }
        let left_out = if others.is_empty() {
            String::new()
        } else {
            format!("; left out {} others ({})", others.len(), shown.join(", "))
        };
        tracing::info!("composio connect: up, {count} tools{left_out}");
        true
    }

    /// `_failed(why)`: one line per failed start: a warning when the reason is new, else a quieter repeat; none
    /// once closing.
    fn failed(&self, why: &str) {
        if *self.stop.borrow() {
            return;
        }
        let mut last = self.last_failure.lock();
        if *last == why {
            tracing::info!("composio connect: the apps are not up yet ({why})");
        } else {
            tracing::warn!("composio connect: the apps are not up yet ({why})");
        }
        *last = why.to_string();
    }

    /// Stop the start task (waiting up to 2 s for it, as Python joins its thread) and close the connections. The
    /// bridge is not used after this.
    pub async fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.stop.send_replace(true);
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let running = self.task.lock().as_ref().is_some_and(|t| !t.is_finished());
            if !running {
                break;
            }
            if Instant::now() >= deadline {
                // a request still in flight: closing the connections ends it, as httpx's close does
                if let Some(t) = self.task.lock().as_ref() {
                    t.abort();
                }
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    // -- calls --

    /// `_authorized(client, call)`: the call with the sign-in. When Composio answers 401, the sign-in is refreshed
    /// once and the call made once more. A 401 to that fresh token too holds the sign-in (the token source's
    /// `hold`): until the hold is over, no call sends it or mints another. SignedOut when there is no token to
    /// send, no refresh, or a second 401. McpClient fails with the same McpError for 401 and 403, so the status is
    /// read off the response itself (`Seen`), as is the token that call sent.
    async fn authorized(&self, lane: &Lane, op: Op<'_>) -> Result<Value, Fail> {
        let _turn = lane.turn.lock().await;
        use eve_google_rest::TokenSource as _;
        if lane.tokens.token().await.filter(|t| !t.is_empty()).is_none() {
            return Err(Fail::SignedOut);
        }
        for attempt in 1..=2 {
            *lane.seen.status.lock() = None;
            let result = match &op {
                Op::List => lane.client.list_tools().await.map(Value::Array),
                Op::Call(name, args) => lane.client.call_tool(name, args).await.map(Value::Object),
            };
            match result {
                Ok(value) => return Ok(value),
                Err(e) if e.kind != MCP_ERROR || *lane.seen.status.lock() != Some(401) => {
                    return Err(Fail::Call(e))
                }
                Err(_) => {}
            }
            let sent = lane.seen.token.lock().clone();
            if attempt == 2 {
                self.tokens.hold(sent.as_deref()).await;
                return Err(Fail::SignedOut);
            }
            if self.tokens.refresh(sent.as_deref()).await.filter(|t| !t.is_empty()).is_none() {
                return Err(Fail::SignedOut);
            }
        }
        Err(Fail::SignedOut)
    }

    /// `execute(name, args)`. Never fails: a failure is an object the model can read and tell the owner about.
    pub async fn execute(&self, name: &str, args: &Obj) -> Obj {
        self.execute_value(name, &Value::Object(args.clone())).await
    }

    /// `execute(name, args)` with the arguments as they came: null is `{}`, anything else that is not an object is
    /// refused.
    pub async fn execute_value(&self, name: &str, args: &Value) -> Obj {
        let mut out = Obj::new();
        out.insert("ok".into(), json!(false));
        let names_up = !self.up.lock().names.is_empty();
        if !self.up.lock().names.contains(name) {
            let reason = if names_up { format!("{name} is not available") } else { NOT_UP.to_string() };
            out.insert("reason".into(), json!(reason));
            return out;
        }
        let arguments = match args {
            Value::Null => Obj::new(),
            Value::Object(m) => m.clone(),
            _ => {
                out.insert("reason".into(), json!("the arguments must be a JSON object"));
                return out;
            }
        };
        let result = match self.authorized(&self.brain, Op::Call(name, &arguments)).await {
            Ok(result) => result,
            Err(Fail::SignedOut) => {
                tracing::warn!("composio connect: {name} not run: no sign-in Composio takes");
                out.insert("reason".into(), json!(self.why()));
                return out;
            }
            Err(Fail::Call(e)) => {
                // the type only: arguments may hold mail text
                tracing::warn!("composio connect: {name} failed: {}", e.kind);
                let mut reason = format!("{}: {}", e.kind, clip(&e.message, 200));
                if MAY_HAVE_RUN_KINDS.contains(&e.kind.as_str()) {
                    reason.push_str(MAY_HAVE_RUN);
                }
                out.insert("reason".into(), json!(reason));
                return out;
            }
        };
        // The call has run. Nothing below may turn its answer into a failure the model would run again.
        let reader = self.readers.lock().result_for_model;
        let read = catch_unwind(AssertUnwindSafe(|| {
            let found = payload(&result);
            reader(&result, found.as_ref())
        }));
        let answer = match read {
            Ok(answer) => answer,
            Err(_) => {
                tracing::warn!("composio connect: {name} ran; its answer was not read (panic)");
                out.insert("reason".into(), json!(UNREADABLE));
                return out;
            }
        };
        if let Some(seen) = answer_object(&result) {
            // Settings' bookkeeping never costs the model its answer
            if catch_unwind(AssertUnwindSafe(|| self.observe(name, &seen))).is_err() {
                tracing::info!("composio connect: {name}'s answer was not read for Settings (panic)");
            }
        }
        answer
    }

    // -- Settings → Apps --

    /// `connect_link(toolkit)`: the Connect Link that connects `toolkit` (a Composio toolkit slug, as Settings →
    /// Apps names it), or None: already connected, not a toolkit eve connects (a malformed slug, or one of the web
    /// toolkits eve never runs), Composio not reachable, or a link that is not on composio.dev (link_ok). Asks
    /// COMPOSIO_MANAGE_CONNECTIONS in the one shape that only checks or links
    /// (eve_composio_tools::manage_needs_yes). Never fails.
    pub async fn connect_link(&self, toolkit: &str) -> Option<String> {
        let slug = toolkit_slug_str(toolkit)?;
        if eve_composio_tools::WEB_TOOLKITS.contains(&slug.as_str()) {
            return None;
        }
        let mut args = Obj::new();
        args.insert("toolkits".into(), json!([slug]));
        let (link, active) = match self.authorized(&self.linker, Op::Call(MANAGE_CONNECTIONS, &args)).await {
            Ok(result) => connect_answer(answer_object(&result).as_ref(), &slug),
            Err(Fail::SignedOut) => {
                tracing::warn!("composio connect: no Connect Link for {slug}: no sign-in Composio takes");
                return None;
            }
            Err(Fail::Call(e)) => {
                tracing::warn!("composio connect: no Connect Link for {slug} ({})", e.kind);
                return None;
            }
        };
        if active == Some(true) {
            self.remember(&[(slug.clone(), true)], true);
            tracing::info!("composio connect: {slug} is already connected");
            return None;
        }
        let Some(link) = link else {
            tracing::warn!("composio connect: Composio sent no Connect Link for {slug}");
            return None;
        };
        if !link_ok_str(&link) {
            tracing::warn!("composio connect: the link Composio sent for {slug} is not on composio.dev; not used");
            return None;
        }
        self.remember(&[(slug, false)], false); // being connected now: read it again next time
        Some(link)
    }

    /// `connections(toolkits, max_age=)`: which of `toolkits` the person has connected in Composio: {slug:
    /// connected}, in their order. Read from COMPOSIO_SEARCH_TOOLS (it only looks), for the apps not read in the
    /// last `max_age` seconds (0: all, now), and from each search and connection check the brain runs. An app
    /// Composio did not report, or any app while Composio cannot be reached, keeps what was last known: false when
    /// nothing was. Never fails. Python's default is `connections(tuple(APPS), max_age=CONNECTIONS_TTL_SECS)`:
    /// [`app_slugs`] and [`CONNECTIONS_TTL_SECS`].
    pub async fn connections(&self, toolkits: &[&str], max_age: f64) -> IndexMap<String, bool> {
        let mut slugs: Vec<String> = Vec::new();
        for t in toolkits {
            if let Some(slug) = toolkit_slug_str(t) {
                if !slugs.contains(&slug) {
                    slugs.push(slug);
                }
            }
        }
        self.connections_of(slugs, max_age).await
    }

    /// [`connections`](Self::connections) for toolkits as any JSON value: a string is one toolkit, a list its
    /// items, an object its keys; anything else (Python's TypeError) none.
    pub async fn connections_value(&self, toolkits: &Value, max_age: f64) -> IndexMap<String, bool> {
        let items: Vec<Value> = match toolkits {
            Value::String(s) => vec![json!(s)],
            Value::Array(items) => items.clone(),
            Value::Object(m) => m.keys().map(|k| json!(k)).collect(),
            _ => Vec::new(),
        };
        let mut slugs: Vec<String> = Vec::new();
        for item in &items {
            if let Some(slug) = toolkit_slug(item) {
                if !slugs.contains(&slug) {
                    slugs.push(slug);
                }
            }
        }
        self.connections_of(slugs, max_age).await
    }

    async fn connections_of(&self, slugs: Vec<String>, max_age: f64) -> IndexMap<String, bool> {
        if !slugs.is_empty() {
            let _one = self.looking.lock().await; // a caller that waited here finds the answer fresh
            let stale = self.stale(&slugs, max_age);
            for chunk in stale.chunks(LOOK_MAX) {
                if !self.look(chunk).await {
                    break;
                }
            }
        }
        let looks = self.looks.lock();
        slugs.into_iter().map(|s| (s.clone(), looks.known.get(&s).copied().unwrap_or(false))).collect()
    }

    fn stale(&self, slugs: &[String], max_age: f64) -> Vec<String> {
        let looks = self.looks.lock();
        let now = self.now();
        if now < looks.look_retry_at {
            return Vec::new();
        }
        slugs
            .iter()
            .filter(|s| now - looks.looked.get(*s).copied().unwrap_or(f64::NEG_INFINITY) >= max_age)
            .cloned()
            .collect()
    }

    /// `_look(slugs)`: read whether `slugs` are connected, in one COMPOSIO_SEARCH_TOOLS call; false when it failed.
    async fn look(&self, slugs: &[String]) -> bool {
        let queries: Vec<Value> = slugs
            .iter()
            .map(|slug| {
                let use_case = LOOKS
                    .iter()
                    .find(|(s, _)| s == slug)
                    .map(|(_, q)| q.to_string())
                    .unwrap_or_else(|| format!("list my recent items in {slug}"));
                json!({"use_case": use_case})
            })
            .collect();
        let mut args = Obj::new();
        args.insert("queries".into(), Value::Array(queries));
        args.insert("session".into(), json!({"generate_id": true}));
        let statuses = self.readers.lock().search_statuses;
        let seen = match self.authorized(&self.looker, Op::Call(SEARCH_TOOLS, &args)).await {
            Ok(result) => {
                let found = answer_object(&result).map(Value::Object).unwrap_or(Value::Null);
                catch_unwind(AssertUnwindSafe(|| statuses(&found))).map_err(|_| "panic".to_string())
            }
            Err(Fail::SignedOut) => Err("signed out".to_string()),
            Err(Fail::Call(e)) => Err(e.kind),
        };
        let seen = match seen {
            Ok(seen) => seen,
            Err(why) => {
                self.looks.lock().look_retry_at = self.now() + LOOK_RETRY_SECS;
                tracing::warn!("composio connect: the connected apps were not read ({why})");
                return false;
            }
        };
        {
            let mut looks = self.looks.lock();
            let now = self.now();
            for (slug, on) in &seen {
                looks.known.insert(slug.clone(), *on);
            }
            for slug in slugs {
                looks.looked.insert(slug.clone(), now); // read, even when Composio did not report it
            }
        }
        let on = slugs.iter().filter(|s| seen.get(*s).copied().unwrap_or(false)).count();
        tracing::info!("composio connect: {on} of {} apps read as connected", slugs.len());
        true
    }

    /// `_remember(seen, fresh=)`: record what was learned of some apps. `fresh`: it is their state now, so
    /// connections() trusts it for CONNECTIONS_TTL_SECS; otherwise it is about to change (a link was just sent), so
    /// they are read next time.
    fn remember(&self, seen: &[(String, bool)], fresh: bool) {
        let mut looks = self.looks.lock();
        let now = self.now();
        for (slug, connected) in seen {
            looks.known.insert(slug.clone(), *connected);
            if fresh {
                looks.looked.insert(slug.clone(), now);
            } else {
                looks.looked.shift_remove(slug);
            }
        }
    }

    /// `_observe(name, payload)`: what a search or a connection check the brain ran says about which apps are
    /// connected (connections()).
    fn observe(&self, name: &str, payload: &Obj) {
        let whole = Value::Object(payload.clone());
        if name == SEARCH_TOOLS {
            let statuses = self.readers.lock().search_statuses;
            let seen: Vec<(String, bool)> = statuses(&whole).into_iter().collect();
            self.remember(&seen, true);
        } else if name == MANAGE_CONNECTIONS {
            for (slug, item) in manage_items(&whole) {
                if let Some(active) = item_active(&item) {
                    self.remember(&[(slug, active)], active);
                }
            }
        }
    }
}
