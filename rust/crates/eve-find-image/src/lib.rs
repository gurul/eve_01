//! A real, high-quality picture of a place, venue, artist or thing, for eve's cards in the app (OpenUI ImageBlock).
//! The port of `find_image.py`.
//!
//! Owner, 2026-10-13: "for openui/generativeui use images when u can, especially for like concerts, places and things
//! like that and a highquality one is available". A card may only show an image URL a tool returned (gen-prompt.mjs
//! RULES), and until now nothing returned one. Concerts get theirs from Ticketmaster (events.py, `image`); this tool
//! covers the rest with Wikipedia's lead picture: one keyless call to the MediaWiki API (search, then the page's
//! original image), kept only when it is a photo-sized JPEG, PNG or WebP of at least `MIN_WIDTH` x `MIN_HEIGHT`.
//!
//! `handle("find_image", {"subject": ...})` → `{"ok": true, "image", "width", "height", "page", "page_url"}`, or
//! `ok: false` with a plain reason (nothing found, only a small or drawn picture, a timeout). Never fails.
//! `EVE_FIND_IMAGE=0` turns it off. The page title is other people's words, so a result taints the turn as
//! web_search's does (tool_gate).

use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::Duration;

use async_trait::async_trait;
use regex::Regex;
use serde_json::{json, Map, Value};

pub const SWITCH: &str = "EVE_FIND_IMAGE";
pub const TOOL_NAME: &str = "find_image";
pub const TOOL_NAMES: [&str; 1] = [TOOL_NAME];
pub const API: &str = "https://en.wikipedia.org/w/api.php";
pub const USER_AGENT: &str = "eve-bridge find_image (personal assistant; https://github.com/gurul/eve_01)";
pub const TIMEOUT_S: f64 = 6.0;
pub const MIN_WIDTH: i64 = 800;
pub const MIN_HEIGHT: i64 = 400;
pub const MAX_SUBJECT_CHARS: usize = 120;
pub const OFF_REASON: &str = "Pictures are off on this Mac (EVE_FIND_IMAGE=0).";

/// Python's `re.compile(r"\.(jpe?g|png|webp)$", re.I)`: `$` also matches before one trailing newline.
static PHOTO: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\.(jpe?g|png|webp)\n?\z").expect("valid regex"));

pub static TOOLS: LazyLock<Vec<Value>> = LazyLock::new(|| {
    vec![json!({
        "type": "function",
        "name": TOOL_NAME,
        "description": concat!(
            "A real, high-quality picture of one place, venue, landmark, city, artist, band, team, dish, ",
            "product or thing, from Wikipedia, to show in a card in the app (ImageBlock, or a ListBlock ",
            "item's image). Returns its URL and size, or ok: false when no good picture exists; then the ",
            "card goes without one. Concerts and shows from events_search already carry their own image."),
        "parameters": {
            "type": "object",
            "properties": {"subject": {"type": "string",
                                       "description": concat!(
                                           "What to picture, by its proper name (\"Climate Pledge Arena\", ",
                                           "\"Pike Place Market\", \"Phoebe Bridgers\").")}},
            "required": ["subject"],
            "additionalProperties": false,
        },
    })]
});

// ---- Python value semantics -------------------------------------------------------------------------------------

/// Python's `str.isspace()` for one character: Unicode White_Space plus the ASCII separators U+001C..U+001F.
fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Python truthiness of a JSON value.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Python's `str(v or "")`.
fn py_str_or_empty(v: Option<&Value>) -> String {
    match v {
        Some(v) if truthy(v) => match v {
            Value::String(s) => s.clone(),
            Value::Bool(true) => "True".to_string(),
            other => other.to_string(),
        },
        _ => String::new(),
    }
}

/// Python's `int(v or 0)` for the values MediaWiki can send; `None` where Python's `int()` would raise.
fn py_int_or_zero(v: Option<&Value>) -> Option<i64> {
    match v {
        Some(v) if truthy(v) => match v {
            Value::Bool(true) => Some(1),
            Value::Number(n) => n.as_i64().or_else(|| n.as_f64().filter(|f| f.is_finite()).map(|f| f.trunc() as i64)),
            Value::String(s) => {
                // int() takes single underscores between digits only.
                let t = s.trim_matches(py_isspace);
                let digits = t.trim_start_matches(['+', '-']);
                if digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
                    return None;
                }
                t.replace('_', "").parse::<i64>().ok()
            }
            _ => None,
        },
        _ => Some(0),
    }
}

// ---- the switch -------------------------------------------------------------------------------------------------

/// On unless `EVE_FIND_IMAGE` is 0/false/no/off. `None` reads the process environment.
pub fn enabled(environ: Option<&HashMap<String, String>>) -> bool {
    let raw = match environ {
        Some(env) => env.get(SWITCH).cloned().unwrap_or_default(),
        None => std::env::var_os(SWITCH).map(|v| v.to_string_lossy().into_owned()).unwrap_or_default(),
    };
    let v = raw.trim_matches(py_isspace).to_lowercase();
    !matches!(v.as_str(), "0" | "false" | "no" | "off")
}

/// The tool schemas the brain is offered: the picture tool when on, else none.
pub fn tools(environ: Option<&HashMap<String, String>>) -> Vec<Value> {
    if enabled(environ) {
        TOOLS.clone()
    } else {
        Vec::new()
    }
}

// ---- the answer -------------------------------------------------------------------------------------------------

static EMPTY: LazyLock<Map<String, Value>> = LazyLock::new(Map::new);

/// Python's `(v or {})` where the result is then used as a dict: `None` for a truthy non-dict (Python would raise).
fn as_obj(v: Option<&Value>) -> Option<&Map<String, Value>> {
    match v {
        Some(v) if truthy(v) => v.as_object(),
        _ => Some(&EMPTY),
    }
}

/// The MediaWiki answer → the first page's original picture when it is good enough, else None.
pub fn pick(payload: &Value) -> Option<Value> {
    let root = as_obj(Some(payload))?;
    let query = as_obj(root.get("query"))?;
    let pages: Vec<&Map<String, Value>> = match query.get("pages") {
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_object).collect(),
        _ => Vec::new(),
    };
    let index = |p: &Map<String, Value>| -> f64 { p.get("index").and_then(Value::as_f64).unwrap_or(0.0) };
    let mut ranked = pages;
    // A stable sort, as Python's sorted().
    ranked.sort_by(|a, b| index(a).partial_cmp(&index(b)).unwrap_or(std::cmp::Ordering::Equal));
    for page in ranked {
        let Some(orig) = as_obj(page.get("original")) else { continue };
        let src = match orig.get("source") {
            Some(Value::String(s)) => s.clone(),
            other => py_str_or_empty(other),
        };
        let (Some(w), Some(h)) = (py_int_or_zero(orig.get("width")), py_int_or_zero(orig.get("height"))) else {
            continue;
        };
        let bare = src.split('?').next().unwrap_or("");
        if !src.starts_with("https://") || !PHOTO.is_match(bare) {
            continue;
        }
        if w < MIN_WIDTH || h < MIN_HEIGHT {
            continue;
        }
        let title = py_str_or_empty(page.get("title"));
        let page_url = format!("https://en.wikipedia.org/wiki/{}", title.replace(' ', "_"));
        return Some(json!({"ok": true, "image": src, "width": w, "height": h, "page": title, "page_url": page_url}));
    }
    None
}

// ---- the call ---------------------------------------------------------------------------------------------------

/// Why the MediaWiki call gave no answer. Only `name` (the kind of failure, as Python logs the exception's class
/// name) reaches the log: the URL carries the owner's subject.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{name}")]
pub struct FetchError {
    pub name: String,
}

impl FetchError {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

/// The seam for the one GET (Python's `http=` parameter): `(url, params)` → the decoded JSON payload.
#[async_trait]
pub trait Http: Send + Sync {
    async fn get_json(&self, url: &str, params: &[(&str, String)]) -> Result<Value, FetchError>;
}

/// The real call: a GET with `TIMEOUT_S` for connecting and for each read (httpx's per-phase timeout), eve's
/// User-Agent, no redirects followed, and any non-2xx answer an error (httpx's `raise_for_status`).
#[derive(Debug, Clone, Copy, Default)]
pub struct ReqwestHttp;

fn reqwest_error_name(e: &reqwest::Error) -> &'static str {
    if e.is_timeout() {
        if e.is_connect() {
            "ConnectTimeout"
        } else {
            "ReadTimeout"
        }
    } else if e.is_connect() {
        "ConnectError"
    } else if e.is_status() || e.is_redirect() {
        "HTTPStatusError"
    } else if e.is_decode() {
        "JSONDecodeError"
    } else if e.is_builder() {
        "InvalidURL"
    } else {
        "TransportError"
    }
}

#[async_trait]
impl Http for ReqwestHttp {
    async fn get_json(&self, url: &str, params: &[(&str, String)]) -> Result<Value, FetchError> {
        let timeout = Duration::from_secs_f64(TIMEOUT_S);
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(timeout)
            .read_timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| FetchError::new(reqwest_error_name(&e)))?;
        let r = client
            .get(url)
            .query(params)
            .send()
            .await
            .map_err(|e| FetchError::new(reqwest_error_name(&e)))?;
        if !r.status().is_success() {
            return Err(FetchError::new("HTTPStatusError"));
        }
        let body = r.bytes().await.map_err(|e| FetchError::new(reqwest_error_name(&e)))?;
        serde_json::from_slice(&body).map_err(|_| FetchError::new("JSONDecodeError"))
    }
}

/// The MediaWiki query for `subject`, in Python's order.
pub fn params(subject: &str) -> Vec<(&'static str, String)> {
    vec![
        ("action", "query".to_string()),
        ("format", "json".to_string()),
        ("formatversion", "2".to_string()),
        ("generator", "search".to_string()),
        ("gsrsearch", subject.to_string()),
        ("gsrlimit", "3".to_string()),
        ("prop", "pageimages".to_string()),
        ("piprop", "original".to_string()),
    ]
}

/// `" ".join(str(args.get("subject") or "").split())[:MAX_SUBJECT_CHARS]`.
fn clean_subject(args: &Value) -> String {
    let raw = py_str_or_empty(args.as_object().and_then(|a| a.get("subject")));
    let joined = raw.split(py_isspace).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ");
    joined.chars().take(MAX_SUBJECT_CHARS).collect()
}

/// Answer one `find_image` call. `environ` `None` reads the process environment; `http` `None` uses `ReqwestHttp`.
pub async fn handle(
    name: &str,
    args: &Value,
    environ: Option<&HashMap<String, String>>,
    http: Option<&dyn Http>,
) -> Value {
    if name != TOOL_NAME {
        return json!({"ok": false, "reason": format!("{name} is not the picture tool")});
    }
    if !enabled(environ) {
        return json!({"ok": false, "reason": OFF_REASON});
    }
    let subject = clean_subject(args);
    if subject.chars().count() < 2 {
        return json!({"ok": false, "reason": "subject must name what to picture"});
    }
    let params = params(&subject);
    let answer = match http {
        Some(h) => h.get_json(API, &params).await,
        None => ReqwestHttp.get_json(API, &params).await,
    };
    let payload = match answer {
        Ok(p) => p,
        Err(e) => {
            // A picture is optional: say so and the card goes without.
            tracing::info!("find_image: no answer ({})", e.name);
            return json!({"ok": false, "reason": "Wikipedia did not answer; leave the picture out"});
        }
    };
    let found = pick(&payload);
    tracing::info!("find_image: {}", if found.is_some() { "found one" } else { "none good enough" });
    found.unwrap_or_else(|| json!({"ok": false, "reason": "no high-quality picture of that; leave the picture out"}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn photo_regex_matches_python() {
        assert!(PHOTO.is_match("a.JPG"));
        assert!(PHOTO.is_match("a.jpeg"));
        assert!(PHOTO.is_match("a.webp\n"));
        assert!(!PHOTO.is_match("a.svg"));
        assert!(!PHOTO.is_match("a.jpg.svg"));
        assert!(!PHOTO.is_match("a.gif"));
    }

    #[test]
    fn subject_is_squeezed_and_clipped() {
        assert_eq!(clean_subject(&json!({"subject": "  Pike \t Place\n Market "})), "Pike Place Market");
        assert_eq!(clean_subject(&json!({"subject": "a".repeat(300)})).chars().count(), MAX_SUBJECT_CHARS);
        assert_eq!(clean_subject(&json!({"subject": null})), "");
        assert_eq!(clean_subject(&json!({"subject": 0})), "");
        assert_eq!(clean_subject(&json!({"subject": 42})), "42");
        assert_eq!(clean_subject(&json!({})), "");
    }

    #[test]
    fn switch_reads_python_falsy_words() {
        for off in ["0", "false", " OFF ", "No"] {
            let env = HashMap::from([(SWITCH.to_string(), off.to_string())]);
            assert!(!enabled(Some(&env)), "{off}");
        }
        for on in ["", "1", "yes", "true"] {
            let env = HashMap::from([(SWITCH.to_string(), on.to_string())]);
            assert!(enabled(Some(&env)), "{on}");
        }
        assert!(enabled(Some(&HashMap::new())));
    }

    #[test]
    fn params_in_python_order() {
        let p = params("Space Needle");
        let keys: Vec<&str> = p.iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, ["action", "format", "formatversion", "generator", "gsrsearch", "gsrlimit", "prop", "piprop"]);
        assert_eq!(p[4].1, "Space Needle");
    }

    #[test]
    fn widths_parse_as_python_int() {
        assert_eq!(py_int_or_zero(Some(&json!(1600.7))), Some(1600));
        assert_eq!(py_int_or_zero(Some(&json!(" 1_600 "))), Some(1600));
        assert_eq!(py_int_or_zero(Some(&json!(null))), Some(0));
        assert_eq!(py_int_or_zero(None), Some(0));
        assert_eq!(py_int_or_zero(Some(&json!("wide"))), None);
    }
}
