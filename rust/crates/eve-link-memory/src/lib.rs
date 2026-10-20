//! eve's memory in the Eve Link app: a Memory card beside Music, Lights and Remote (docs/app-chat.md, "Work").
//! Port of `link_memory.py`.
//!
//! Owner, 2026-10-09: "make memories accessible through app". The card is the chat's own memory tools (memory.py:
//! `search`, `forget_preview`, `forget_apply`, and the profile the brain reads), reached the way the other cards
//! are: `POST /api/link/controls/memory` with one JSON body, answered as JSON (link_chat.LinkChat._controls). Nothing
//! new is stored; nothing is forgotten without the two-step the chat has (a preview that counts, then the token).
//!
//! Bodies:
//!
//! * `{"action": "profile"}` (or no body): `{"ok", "profile"}`, what eve knows about the owner (the one-pager).
//! * `{"action": "search", "query"}`: `{"ok", "hits", "recalled", "said", "note"?}` as `Memory.search` returns
//!   them: record facts `[{id, line, src?}]`, recalled meanings `[str]`, and the words said
//!   `[{day, time, ch, who, text}]` (the line before and after are left out: a phone screen).
//! * `{"action": "forget", "query"}`: `Memory.forget_preview`: `{"ok", "total", "layers", "token", "line"}`;
//!   the token is good once for ten minutes. Counts, never the words.
//! * `{"action": "apply", "token"}`: `Memory.forget_apply`: `{"ok", "total", "layers", "line"}`.
//! * `{"action": "remember", "text"}`: `Memory.star`, the chat's "remember that": one claim kept for good, on the
//!   profile from the next message (owner, 2026-10-09: "add all of these to memories"). `{"ok", "line"}`.
//! * `{"action": "hide", "text"}`: `Memory.hide`, ChatGPT's "Don't mention this again": every line the words match
//!   is left out of the profile and every search until unhidden; nothing is deleted. `{"ok", "line"}`.
//! * `{"action": "unhide", "text"}`: `Memory.unhide`, the hidden line taken back. `{"ok", "line"}`.
//! * `{"action": "hidden"}`: `{"ok", "hidden": [str]}`, what is hidden, oldest first.
//!
//! `available` is false while memory is off (EVE_MEMORY) or not built yet; the controls listing says so and the
//! app shows no card. The memory object is read at each call (`getter`): the daemon builds it after the server.

pub mod py;

use std::sync::Arc;

use anyhow::{anyhow, Result};
use serde_json::{json, Map, Value};
use tracing::info;

pub const MAX_QUERY_CHARS: usize = 200;
pub const MAX_TOKEN_CHARS: usize = 64;
pub const MAX_REMEMBER_CHARS: usize = 500;
pub const MAX_HIDE_CHARS: usize = 500;
pub const OFF_LINE: &str = "eve's memory is off on this Mac (EVE_MEMORY).";
pub const ACTIONS: [&str; 8] = ["profile", "search", "forget", "apply", "remember", "hide", "unhide", "hidden"];

/// What the Memory card calls on `memory.Memory`. The methods are blocking (SQLite, files, the index), so the
/// panel runs each on the blocking pool, as the Python runs them with `asyncio.to_thread`. An `Err` is the
/// exception the Python method would raise; it propagates out of [`MemoryPanel::panel`] to the controls route,
/// which answers 500.
///
/// `search`, `forget_preview` and `forget_apply` return the free-form dicts `memory.py` returns, as JSON.
pub trait MemoryApi: Send + Sync {
    /// `Memory.profile()`: the one-pager the brain reads ("" or `None` when there is none yet).
    fn profile(&self) -> Result<Option<String>>;
    /// `Memory.search(query)`: `{"ok"?, "hits", "recalled", "said", "note"?, "reason"?}`.
    fn search(&self, query: &str) -> Result<Value>;
    /// `Memory.forget_preview(query)`: `{"ok", "total", "layers", "token", "expires_in", "reason"?}`.
    fn forget_preview(&self, query: &str) -> Result<Value>;
    /// `Memory.forget_apply(token)`: `{"ok", "total"?, "layers"?, "reason"?}`.
    fn forget_apply(&self, token: &str) -> Result<Value>;
    /// `Memory.star(text)`: the kept line, or `None` when refused.
    fn star(&self, text: &str) -> Result<Option<String>>;
    /// `Memory.hide(text)`: the hidden line, or `None` when refused.
    fn hide(&self, text: &str) -> Result<Option<String>>;
    /// `Memory.unhide(text)`: whether a hidden line was taken back.
    fn unhide(&self, text: &str) -> Result<bool>;
    /// `Memory.hidden()`: what is hidden, oldest first.
    fn hidden(&self) -> Result<Vec<String>>;
}

/// The getter: the memory object as it is now, `None` while memory is off or not built. `Err` is the getter
/// raising.
pub type Getter = Arc<dyn Fn() -> Result<Option<Arc<dyn MemoryApi>>> + Send + Sync>;

/// The Memory card's `panel(body)`, over `getter()` -> memory.Memory or None.
#[derive(Clone)]
pub struct MemoryPanel {
    getter: Getter,
}

impl std::fmt::Debug for MemoryPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryPanel").field("available", &self.available()).finish()
    }
}

/// `asyncio.to_thread(f, memory)`: runs a blocking memory call off the runtime. A panic in it is an error, as an
/// exception in the thread is in the Python.
async fn to_thread<T, F>(memory: &Arc<dyn MemoryApi>, f: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce(&dyn MemoryApi) -> Result<T> + Send + 'static,
{
    let m = Arc::clone(memory);
    tokio::task::spawn_blocking(move || f(m.as_ref()))
        .await
        .map_err(|e| anyhow!("the memory call failed: {e}"))?
}

fn obj(v: &Value) -> Result<&Map<String, Value>> {
    v.as_object().ok_or_else(|| anyhow!("AttributeError: the memory answered with something other than a dict"))
}

fn things(total: i64) -> &'static str {
    if total == 1 {
        "thing"
    } else {
        "things"
    }
}

impl MemoryPanel {
    /// A panel over a getter that cannot fail: `MemoryPanel(lambda: memory)`.
    pub fn new<F>(getter: F) -> Self
    where
        F: Fn() -> Option<Arc<dyn MemoryApi>> + Send + Sync + 'static,
    {
        Self { getter: Arc::new(move || Ok(getter())) }
    }

    /// A panel over a getter that may raise.
    pub fn with_getter(getter: Getter) -> Self {
        Self { getter }
    }

    /// Whether the card is listed: the memory is built. A getter that fails counts as not built.
    pub fn available(&self) -> bool {
        matches!((self.getter)(), Ok(Some(_)))
    }

    /// One request from the card: `body` is the POSTed JSON object. `Err` is an exception the Python would raise
    /// (the getter's, the memory's, or a memory answer of the wrong shape).
    pub async fn panel(&self, body: &Value) -> Result<Value> {
        let memory = match (self.getter)()? {
            Some(m) => m,
            None => return Ok(json!({"ok": false, "line": OFF_LINE})),
        };
        let action = py::str_or(py::get(body, "action"), "profile");
        if !ACTIONS.contains(&action.as_str()) {
            return Ok(json!({"ok": false, "line": "That isn't something the Memory card does."}));
        }
        if action == "profile" {
            let text = to_thread(&memory, |m| m.profile()).await?.unwrap_or_default();
            let line = if text.is_empty() { "eve has no profile of you yet." } else { "" };
            return Ok(json!({"ok": true, "profile": text, "line": line}));
        }
        if action == "apply" {
            let raw = py::str_or_empty(py::get(body, "token"));
            let token = py::take_chars(py::strip(&raw), MAX_TOKEN_CHARS).to_string();
            let out = to_thread(&memory, move |m| m.forget_apply(&token)).await?;
            return Self::forgotten(&out);
        }
        if action == "remember" {
            let squashed = py::squash_ws(&py::str_or_empty(py::get(body, "text")));
            let text = py::take_chars(&squashed, MAX_REMEMBER_CHARS).to_string();
            if text.chars().count() < 4 {
                return Ok(json!({"ok": false, "line": "Type what to remember."}));
            }
            let kept = to_thread(&memory, move |m| m.star(&text)).await?.is_some_and(|s| !s.is_empty());
            info!("link memory: remember → {}", if kept { "kept" } else { "refused" });
            let line = if kept { "Remembered." } else { "That couldn't be kept just now." };
            return Ok(json!({"ok": kept, "line": line}));
        }
        if action == "hidden" {
            let rows = to_thread(&memory, |m| m.hidden()).await?;
            info!("link memory: hidden → {}", rows.len());
            return Ok(json!({"ok": true, "hidden": rows, "line": ""}));
        }
        if action == "hide" || action == "unhide" {
            let squashed = py::squash_ws(&py::str_or_empty(py::get(body, "text")));
            let text = py::take_chars(&squashed, MAX_HIDE_CHARS).to_string();
            if text.is_empty() {
                let line = if action == "hide" { "Nothing to hide." } else { "Nothing to unhide." };
                return Ok(json!({"ok": false, "line": line}));
            }
            if action == "hide" {
                let kept = to_thread(&memory, move |m| m.hide(&text)).await?.is_some_and(|s| !s.is_empty());
                info!("link memory: hide → {}", if kept { "kept" } else { "refused" });
                let line = if kept { "eve won't bring that up again." } else { "That couldn't be hidden just now." };
                return Ok(json!({"ok": kept, "line": line}));
            }
            let gone = to_thread(&memory, move |m| m.unhide(&text)).await?;
            info!("link memory: unhide → {}", if gone { "done" } else { "none" });
            let line = if gone { "eve can bring that up again." } else { "That wasn't hidden." };
            return Ok(json!({"ok": gone, "line": line}));
        }
        let squashed = py::squash_ws(&py::str_or_empty(py::get(body, "query")));
        let query = py::take_chars(&squashed, MAX_QUERY_CHARS).to_string();
        if query.is_empty() {
            return Ok(json!({"ok": false, "line": "Type a word or two to search for."}));
        }
        if action == "search" {
            let out = to_thread(&memory, move |m| m.search(&query)).await?;
            let o = obj(&out)?;
            // `out.get("ok", True)`: a search that says nothing about ok went fine
            if o.contains_key("ok") && !py::truthy(o.get("ok")) {
                return Ok(json!({"ok": false, "line": py::str_or(o.get("reason"), "That search didn't work.")}));
            }
            let said: Vec<Value> = py::iter_or_empty(o.get("said"))?
                .into_iter()
                .filter_map(|h| match h {
                    Value::Object(h) => {
                        let mut row = Map::new();
                        for k in ["day", "time", "ch", "who", "text"] {
                            row.insert(k.to_string(), h.get(k).cloned().unwrap_or_else(|| json!("")));
                        }
                        Some(Value::Object(row))
                    }
                    _ => None,
                })
                .collect();
            let hits = py::iter_or_empty(o.get("hits"))?;
            let recalled: Vec<Value> =
                py::iter_or_empty(o.get("recalled"))?.iter().map(|r| Value::String(py::py_str(r))).collect();
            info!(
                "link memory: search → {} hits, {} recalled, {} said",
                hits.len(),
                recalled.len(),
                said.len()
            );
            return Ok(json!({"ok": true, "hits": hits, "recalled": recalled, "said": said,
                             "line": py::str_or_empty(o.get("note"))}));
        }
        let out = to_thread(&memory, move |m| m.forget_preview(&query)).await?;
        let o = obj(&out)?;
        if !py::truthy(o.get("ok")) {
            return Ok(json!({"ok": false, "line": py::str_or(o.get("reason"), "That didn't work.")}));
        }
        let total = py::int_or_zero(o.get("total"))?;
        let line = if total == 0 {
            "nothing in memory matches".to_string()
        } else {
            format!("{total} {} match, in {}", things(total), layers(o.get("layers")))
        };
        info!("link memory: forget preview → {total}");
        Ok(json!({"ok": true, "total": total, "layers": py::dict_or_empty(o.get("layers"))?,
                  "token": py::str_or_empty(o.get("token")), "expires_in": py::int_or_zero(o.get("expires_in"))?,
                  "line": line}))
    }

    /// `MemoryPanel._forgotten(out)`: `forget_apply`'s answer as the card shows it.
    pub fn forgotten(out: &Value) -> Result<Value> {
        let o = obj(out)?;
        let total = py::int_or_zero(o.get("total"))?;
        if !py::truthy(o.get("ok")) {
            return Ok(json!({"ok": false, "total": total, "layers": py::dict_or_empty(o.get("layers"))?,
                             "line": py::str_or(o.get("reason"), "That didn't work.")}));
        }
        info!("link memory: forgot {total}");
        let line =
            if total != 0 { format!("Forgot {total} {}.", things(total)) } else { "Nothing to forget.".to_string() };
        Ok(json!({"ok": true, "total": total, "layers": py::dict_or_empty(o.get("layers"))?, "line": line}))
    }
}

/// `_layers(layers)`: the layers a forget touches, in the owner's words ("facts, what was said"); anything but a
/// dict, or a dict with nothing in it, is "memory".
pub fn layers(layers: Option<&Value>) -> String {
    let Some(Value::Object(map)) = layers else {
        return "memory".to_string();
    };
    let name = |k: &str| -> String {
        match k {
            "records" => "facts",
            "index" => "meanings",
            "transcripts" => "what was said",
            "chat_log" => "the chat",
            "archive" => "notes",
            "meetings" => "meetings",
            other => other,
        }
        .to_string()
    };
    let parts: Vec<String> = map.iter().filter(|(_, v)| py::truthy(Some(v))).map(|(k, _)| name(k)).collect();
    if parts.is_empty() {
        "memory".to_string()
    } else {
        parts.join(", ")
    }
}
