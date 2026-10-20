//! The owner's own GitHub repositories, by name and what each is, so "my car repo" is found.
//!
//! Owner, 2026-10-09 ("tune it", GitHub routing): two blind rounds of wording changes failed (19/26 and 17/26
//! against a bar of 24). The misses were requests that name a project the brain had never heard of ("the recipe
//! app", "the mood tracker"): with no repo word in them it could not tell a GitHub repository from a Linear
//! project, a note or an app eve built, so it guessed. The brain is now told the owner's repositories themselves,
//! and a repository's own name in a message opens GitHub like any GitHub word.
//!
//! * **Where from.** GitHub's own search through the direct app's MCP server (`search_repositories` with
//!   `user:<login>`, the login from `get_me`): the owner's repositories, private ones included, as their
//!   fine-grained token sees them. No other request, no other key.
//! * **Kept.** `~/.config/eve-bridge/github-repos.json` (0600), refreshed once a day when the apps start
//!   (`REFRESH_SECS`). A failed refresh keeps the last list; none at all leaves the brain as it was.
//! * **Shown.** The `SHOWN` most recently pushed, each as `name (what it is)`, the description clipped to
//!   `DESC_CLIP` characters, in one line of the apps block (`AppsBridge::instructions`). Every name in it is one
//!   GitHub returned: nothing is made up (the owner's rule for prompts).
//! * **Words.** `names_repo`: a message holding a repository's name as a word (`zorbcar`, `plantpal-x`) names
//!   GitHub. Names shorter than `MIN_NAME`, or that are everyday words (`WORDS`, and any word in macOS's own
//!   `/usr/share/dict/words`), are left out, so "remix" or "leap" in a sentence never opens GitHub, whose schemas
//!   are the largest of the apps; their line still teaches the brain.
//!
//! The names and descriptions ride the brain's prompt, which goes to the model provider like the rest of the
//! turn. Logs carry counts only.

use std::collections::{BTreeSet, HashSet};
use std::path::Path;
use std::sync::{LazyLock, OnceLock};

use async_trait::async_trait;
use eve_google_rest::py::{
    py_str, rstrip, split_ws, squash_ws, strip, take_chars, truthy, type_name,
};
use regex::Regex;
use serde_json::{json, Map, Value};

use crate::apps_mcp::{write_private_json, CallError};
use crate::py::py_float;

pub const REFRESH_SECS: f64 = 24.0 * 3600.0;
/// GitHub's page size: the owner's repositories, most recently pushed first
pub const FETCH_MAX: i64 = 100;
/// in the prompt
pub const SHOWN: usize = 25;
pub const DESC_CLIP: usize = 50;
pub const MIN_NAME: usize = 4;
/// Repository names that are also everyday words: kept in the brain's line, never a keyword.
pub const WORDS: [&str; 23] = [
    "atlas",
    "double",
    "candid",
    "lily",
    "eve",
    "echoes",
    "storeybox",
    "harbor",
    "notes",
    "site",
    "website",
    "blog",
    "app",
    "bot",
    "test",
    "demo",
    "home",
    "config",
    "dotfiles",
    "resume",
    "delphi",
    "handyman",
    "sushi",
];
pub const FILE: &str = "github-repos.json";
pub const DICTIONARY: &str = "/usr/share/dict/words";

static DICTIONARY_WORDS: OnceLock<HashSet<String>> = OnceLock::new();

/// macOS's word list, lower-cased, read once; empty where there is none.
fn english() -> &'static HashSet<String> {
    DICTIONARY_WORDS.get_or_init(|| match std::fs::read(DICTIONARY) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => split_ws(&text)
                .into_iter()
                .map(|w| strip(w).to_lowercase())
                .collect(),
            // read_text(encoding="utf-8") raises a ValueError there, which the Python does not catch; an empty
            // list keeps the names out of nothing and the apps up
            Err(_) => HashSet::new(),
        },
        Err(_) => HashSet::new(),
    })
}

/// One of the owner's repositories: its name and what it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    pub name: String,
    pub description: String,
}

impl Repo {
    pub fn new(name: &str, description: &str) -> Self {
        Self {
            name: name.to_string(),
            description: description.to_string(),
        }
    }

    fn to_json(&self) -> Value {
        json!({"name": self.name, "description": self.description})
    }
}

/// `call(name, args)` in AppsBridge::execute's shape (`{"ok": ..., "result": ...}`). A plain closure is one.
#[async_trait]
pub trait Call: Send + Sync {
    async fn call(&self, name: &str, args: &Map<String, Value>) -> Result<Value, CallError>;
}

#[async_trait]
impl<F> Call for F
where
    F: Fn(&str, &Map<String, Value>) -> Value + Send + Sync,
{
    async fn call(&self, name: &str, args: &Map<String, Value>) -> Result<Value, CallError> {
        Ok(self(name, args))
    }
}

fn parse(result: Option<&Value>) -> Value {
    match result {
        Some(Value::String(s)) => crate::py::json_loads(s).unwrap_or_else(|_| json!({})),
        None | Some(Value::Null) => json!({}),
        Some(other) => other.clone(),
    }
}

fn attr_error(v: &Value) -> CallError {
    CallError::new(
        "AttributeError",
        format!("'{}' object has no attribute 'get'", type_name(v)),
    )
}

/// `value.get(key)` in Python: an AttributeError when the value is not a dict.
fn get<'a>(v: &'a Value, key: &str) -> Result<Option<&'a Value>, CallError> {
    match v {
        Value::Object(m) => Ok(m.get(key)),
        other => Err(attr_error(other)),
    }
}

static LOGIN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9-]{1,39}$").expect("LOGIN"));

/// The owner's repositories through the GitHub app's tools, most recently pushed first, as [{name,
/// description}]; None when GitHub could not say.
pub async fn fetch(call: &dyn Call) -> Result<Option<Vec<Repo>>, CallError> {
    let me = call.call("github__get_me", &Map::new()).await?;
    let login = if get(&me, "ok")?.is_some_and(truthy) {
        get(&parse(get(&me, "result")?), "login")?.cloned()
    } else {
        None
    };
    let Some(login) = login.filter(truthy) else {
        return Ok(None);
    };
    let login = py_str(&login);
    if !LOGIN.is_match(&login) {
        return Ok(None);
    }
    let mut args = Map::new();
    args.insert(
        "query".into(),
        json!(format!("user:{login} fork:true sort:updated")),
    );
    args.insert("perPage".into(), json!(FETCH_MAX));
    let found = call.call("github__search_repositories", &args).await?;
    if !get(&found, "ok")?.is_some_and(truthy) {
        return Ok(None);
    }
    let body = parse(get(&found, "result")?);
    let items: Vec<Value> = match &body {
        Value::Object(m) => {
            let picked = m
                .get("items")
                .filter(|v| truthy(v))
                .or_else(|| m.get("repositories").filter(|v| truthy(v)));
            match picked {
                None => Vec::new(),
                Some(Value::Array(a)) => a.clone(),
                // a dict's keys and a string's characters: none of them a dict
                Some(Value::Object(_)) | Some(Value::String(_)) => Vec::new(),
                Some(other) => {
                    return Err(CallError::new(
                        "TypeError",
                        format!("'{}' object is not iterable", type_name(other)),
                    ))
                }
            }
        }
        _ => Vec::new(),
    };
    let mut out = Vec::new();
    for it in &items {
        let Value::Object(it) = it else {
            continue;
        };
        let name = match it.get("name").filter(|v| truthy(v)) {
            Some(n) => py_str(n),
            None => {
                let full = it
                    .get("full_name")
                    .filter(|v| truthy(v))
                    .map(py_str)
                    .unwrap_or_default();
                full.rsplit('/').next().unwrap_or("").to_string()
            }
        };
        let name = strip(&name).to_string();
        if !name.is_empty() {
            let desc = it
                .get("description")
                .filter(|v| truthy(v))
                .map(py_str)
                .unwrap_or_default();
            out.push(Repo {
                name,
                description: squash_ws(&desc),
            });
        }
    }
    Ok(Some(out))
}

/// (repositories, when fetched) from the kept file; ([], 0) when there is none or it cannot be read.
pub fn load(path: &Path) -> (Vec<Repo>, f64) {
    fn read(path: &Path) -> Option<(Vec<Repo>, f64)> {
        let text = String::from_utf8(std::fs::read(path).ok()?).ok()?;
        let Value::Object(data) = crate::py::json_loads(&text).ok()? else {
            return None;
        };
        let repos: Vec<Value> = match data.get("repos").filter(|v| truthy(v)) {
            None => Vec::new(),
            Some(Value::Array(a)) => a.clone(),
            Some(Value::Object(_)) | Some(Value::String(_)) => Vec::new(),
            Some(_) => return None,
        };
        let repos = repos
            .iter()
            .filter_map(Value::as_object)
            .filter(|r| r.get("name").is_some_and(truthy))
            .map(|r| Repo {
                name: py_str(&r["name"]),
                description: r
                    .get("description")
                    .filter(|v| truthy(v))
                    .map(py_str)
                    .unwrap_or_default(),
            })
            .collect();
        let fetched = py_float(
            data.get("fetched")
                .filter(|v| truthy(v))
                .unwrap_or(&json!(0)),
        )
        .ok()?;
        Some((repos, fetched))
    }
    read(path).unwrap_or((Vec::new(), 0.0))
}

/// The kept list, fetched again when it is older than REFRESH_SECS. A failed fetch keeps the old list. Fails
/// only when the fresh list cannot be written.
pub async fn refresh(
    path: &Path,
    call: &dyn Call,
    clock: &(dyn Fn() -> f64 + Send + Sync),
) -> Result<Vec<Repo>, CallError> {
    let (repos, fetched) = load(path);
    if !repos.is_empty() && clock() - fetched < REFRESH_SECS {
        return Ok(repos);
    }
    let fresh = match fetch(call).await {
        Ok(f) => f,
        Err(e) => {
            // the type only
            tracing::warn!("github repos: not refreshed ({})", e.type_name());
            None
        }
    };
    let Some(fresh) = fresh else {
        tracing::info!("github repos: kept {} (refresh failed)", repos.len());
        return Ok(repos);
    };
    let mut data = Map::new();
    data.insert("fetched".into(), json!(clock()));
    data.insert(
        "repos".into(),
        Value::Array(fresh.iter().map(Repo::to_json).collect()),
    );
    write_private_json(path, &data)?;
    tracing::info!("github repos: {}", fresh.len());
    Ok(fresh)
}

/// The apps block's line: the owner's most recently pushed repositories, each with what it is; "" for none.
pub fn line(repos: &[Repo]) -> String {
    let shown: Vec<String> = repos
        .iter()
        .take(SHOWN)
        .map(|r| {
            let desc = if r.description.chars().count() <= DESC_CLIP {
                r.description.clone()
            } else {
                format!("{}…", rstrip(take_chars(&r.description, DESC_CLIP - 1)))
            };
            if desc.is_empty() {
                r.name.clone()
            } else {
                format!("{} ({desc})", r.name)
            }
        })
        .collect();
    if shown.is_empty() {
        return String::new();
    }
    format!(
        "The owner's own GitHub repositories, most recently worked on first (a project they name loosely, \
         \"my car repo\", \"the scam detector\", is one of these when it fits; read it with GitHub): {}.",
        shown.join("; ")
    )
}

static NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z0-9][a-z0-9._-]*$").expect("NAME"));
static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[a-z0-9][a-z0-9._-]*").expect("WORD"));

/// The repository names that open GitHub when a message holds one as a word.
pub fn keyword_names(repos: &[Repo]) -> BTreeSet<String> {
    keyword_names_in(repos, english())
}

/// `keyword_names` against a given dictionary (the Mac's in the daemon).
pub fn keyword_names_in(repos: &[Repo], dictionary: &HashSet<String>) -> BTreeSet<String> {
    repos
        .iter()
        .map(|r| r.name.to_lowercase())
        .filter(|name| {
            name.chars().count() >= MIN_NAME
                && !WORDS.contains(&name.as_str())
                && !dictionary.contains(name)
                && NAME.is_match(name)
        })
        .collect()
}

/// Whether `text` holds one of `names` as a whole word (case ignored).
pub fn names_repo(text: &str, names: &BTreeSet<String>) -> bool {
    if names.is_empty() || text.is_empty() {
        return false;
    }
    let lowered = text.to_lowercase();
    let mut words: HashSet<String> = WORD
        .find_iter(&lowered)
        .map(|m| m.as_str().to_string())
        .collect();
    // A name that ends a sentence comes with its full stop ("zorbcar."): try each word without trailing marks too
    let trimmed: Vec<String> = words
        .iter()
        .map(|w| w.trim_end_matches(['.', '_', '-']).to_string())
        .collect();
    words.extend(trimmed);
    words.iter().any(|w| names.contains(w))
}
