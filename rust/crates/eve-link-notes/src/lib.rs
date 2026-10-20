//! The owner's notes in the Eve Link app: a Notes app beside Music, Lights and Remote (docs/app-chat.md, "Work").
//!
//! Port of `bridge/src/eve_bridge/link_notes.py`.
//!
//! Owner, 2026-10-10: "an app where I can look at my notes and interact with them change delete updates", "essentially
//! like a mini notion", "make sure it's mobile native". The notes are the second brain (`eve-second-brain`): the
//! markdown vault the owner texts eve into and keeps open in Obsidian. The app draws it natively; this is its panel,
//! reached as the other cards are: `POST /api/link/controls/notes` with one JSON body, answered as JSON
//! (link_chat's `_controls`).
//!
//! Bodies:
//!
//! * `{"action": "list"}` (or no body): `{"ok", "notes": [{path, folder, title, preview, modified}], "folders"}`,
//!   every note newest first (the archive too), and the folders a note can be started in or moved to.
//! * `{"action": "read", "path"}`: `{"ok", "path", "title", "body", "front", "revision", "modified", "editable",
//!   "undo_id"?}`. `body` is the text under the frontmatter, `front` the frontmatter's `key: value` lines (shown,
//!   never edited from the phone). A note too long to save in one request is `editable: false`.
//! * `{"action": "save", "path", "revision", "body"}`: the body replaced, the frontmatter kept (`write_note`);
//!   `{"ok", "path", "revision", "undo_id", "changed"}`. A revision that is not the file's now is refused: the note
//!   changed on the Mac since the phone read it.
//! * `{"action": "undo", "path", "revision", "undo_id"}`: the save taken back (`undo_note`).
//! * `{"action": "create", "folder", "title", "body"?}`: a new page, `# <title>` on top (`create_note`).
//! * `{"action": "delete", "path", "revision"}`: into the vault's hidden trash (`trash_note`); `{"ok",
//!   "trash_id"}`. `{"action": "restore", "trash_id"}` puts it back. Nothing is unlinked.
//! * `{"action": "move", "path", "into"}`: into another folder of `folders` (`move_note`).
//! * `{"action": "search", "query"}`: `{"ok", "hits": [{path, title, snippet}]}`, the brain's own keyword search.
//!
//! `available` is false while the second brain is off (EVE_SECOND_BRAIN) or the vault is not there; the
//! controls listing says so and the app shows no Notes. Counts and file names go to the log, never a note's words.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use eve_second_brain::{self as sb, py, BrainError, VaultConfig};
use serde_json::{json, Map, Value};

pub const ACTIONS: [&str; 9] = ["list", "read", "save", "undo", "create", "delete", "restore", "move", "search"];
/// A save is one request, and link_chat takes 64,000 bytes of JSON (MAX_BODY_BYTES).
pub const MAX_BODY_CHARS: usize = 40_000;
/// A page, not a database dump.
pub const MAX_READ_CHARS: usize = 400_000;
pub const MAX_QUERY_CHARS: usize = 200;
pub const MAX_PATH_CHARS: usize = 400;
pub const MAX_ID_CHARS: usize = 64;
pub const OFF_LINE: &str = "Your notes are off on this Mac (EVE_SECOND_BRAIN).";
pub const CHANGED_LINE: &str = "This note changed on the Mac. Open it again to see the new version.";
pub const TOO_LONG_LINE: &str = "This note is too long to save from the phone. Edit it on the Mac.";
pub const NOT_A_NOTE: &str = "That note isn't there any more.";

/// What reads the vault's configuration, per call (Python's `config` callable).
pub type ConfigFn = Arc<dyn Fn() -> VaultConfig + Send + Sync>;

/// The Notes app's `panel(body)`, over `config()` -> [`VaultConfig`] (read per call).
#[derive(Clone)]
pub struct NotesPanel {
    config: ConfigFn,
}

impl Default for NotesPanel {
    /// Python's default: `second_brain.configured`, which reads the process environment.
    fn default() -> Self {
        Self::new(Arc::new(|| sb::configured(None)))
    }
}

impl NotesPanel {
    pub fn new(config: ConfigFn) -> Self {
        Self { config }
    }

    fn root(&self) -> Option<PathBuf> {
        // Python catches any exception from config(); a panic is the Rust form of one.
        let cfg = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.config)())).ok()?;
        (cfg.enabled && cfg.root.is_dir()).then_some(cfg.root)
    }

    /// False while the second brain is off or its vault folder is not there.
    pub fn available(&self) -> bool {
        self.root().is_some()
    }

    /// One request from the app, answered as JSON. Refusals are `{"ok": false, "line": …}` said to the owner. `Err`
    /// is what Python lets escape (an exception that is neither a ValueError nor an OSError), which link_chat turns
    /// into its 500 answer.
    pub async fn panel(&self, body: &Map<String, Value>) -> Result<Value, BrainError> {
        let Some(root) = self.root() else {
            return Ok(json!({"ok": false, "line": OFF_LINE}));
        };
        let action = match body.get("action").filter(|v| sb::truthy(v)) {
            Some(v) => sb::value_str(v),
            None => "list".to_string(),
        };
        if !ACTIONS.contains(&action.as_str()) {
            return Ok(json!({"ok": false, "line": "That isn't something Notes does."}));
        }
        let owned = body.clone();
        let act = action.clone();
        let result = tokio::task::spawn_blocking(move || do_action(&root, &act, &owned))
            .await
            .map_err(|e| BrainError::Other { kind: "RuntimeError", message: e.to_string() })?;
        let mut out = match result {
            Ok(out) => out,
            Err(BrainError::Value(reason)) => return Ok(json!({"ok": false, "line": owner_line(&reason)})),
            Err(e @ BrainError::Os { .. }) => {
                tracing::warn!("link notes: {} failed ({})", action, e.type_name());
                return Ok(json!({"ok": false, "line": "That didn't work on the Mac just now."}));
            }
            Err(e) => return Err(e),
        };
        if let Some(obj) = out.as_object_mut() {
            obj.entry("line").or_insert(json!(""));
        }
        Ok(out)
    }
}

/// `{**out, "line": line}`: the line set (replaced in place, or added last).
fn with_line(mut out: Value, line: &str) -> Value {
    if let Some(obj) = out.as_object_mut() {
        obj.insert("line".to_string(), json!(line));
    }
    out
}

/// `str(body.get(key) or "")`.
fn arg(body: &Map<String, Value>, key: &str) -> String {
    body.get(key).filter(|v| sb::truthy(v)).map(sb::value_str).unwrap_or_default()
}

fn do_action(root: &Path, action: &str, body: &Map<String, Value>) -> Result<Value, BrainError> {
    if action == "list" {
        let notes = sb::list_notes(root)?;
        tracing::info!("link notes: list → {}", notes.len());
        return Ok(json!({"ok": true, "notes": notes, "folders": sb::note_folders(root)?}));
    }
    if action == "search" {
        let collapsed = py::collapse_ws(&arg(body, "query"));
        let query = py::clip(&collapsed, MAX_QUERY_CHARS);
        if query.is_empty() {
            return Ok(json!({"ok": false, "line": "Type a word or two to search for."}));
        }
        let hits = sb::search(root, query, 30, true)?;
        tracing::info!("link notes: search → {}", hits.len());
        let rows: Vec<Value> =
            hits.iter().map(|h| json!({"path": h.path, "title": h.title, "snippet": h.snippet})).collect();
        let line = if hits.is_empty() { "No note has those words." } else { "" };
        return Ok(json!({"ok": true, "hits": rows, "line": line}));
    }
    if action == "create" {
        let text = arg(body, "body");
        if py::char_len(&text) > MAX_BODY_CHARS {
            return Ok(json!({"ok": false, "line": TOO_LONG_LINE}));
        }
        let folder = match body.get("folder").filter(|v| sb::truthy(v)) {
            Some(v) => sb::value_str(v),
            None => sb::INBOX_DIR.to_string(),
        };
        return sb::create_note(root, py::clip(&folder, MAX_PATH_CHARS), &arg(body, "title"), &text);
    }
    if action == "restore" {
        let out = sb::restore_note(root, &short(body.get("trash_id"), MAX_ID_CHARS))?;
        return Ok(with_line(out, "Restored."));
    }
    let path = short(body.get("path"), MAX_PATH_CHARS);
    if path.is_empty() {
        return Ok(json!({"ok": false, "line": "Which note?"}));
    }
    if action == "read" {
        return read(root, &path);
    }
    if action == "move" {
        let out = sb::move_note(root, &path, &short(body.get("into"), MAX_PATH_CHARS))?;
        let line = if out.get("changed").is_some_and(sb::truthy) { "Moved." } else { "" };
        return Ok(with_line(out, line));
    }
    let revision = short(body.get("revision"), MAX_ID_CHARS);
    if action == "delete" {
        return Ok(with_line(sb::trash_note(root, &path, &revision)?, "Deleted."));
    }
    if action == "undo" {
        let out = sb::undo_note(root, &path, &short(body.get("undo_id"), MAX_ID_CHARS * 2), &revision)?;
        return Ok(with_line(out, "Undone."));
    }
    let Some(Value::String(text)) = body.get("body") else {
        return Ok(json!({"ok": false, "line": "Nothing to save."}));
    };
    if py::char_len(text) > MAX_BODY_CHARS {
        return Ok(json!({"ok": false, "line": TOO_LONG_LINE}));
    }
    sb::write_note(root, &path, &revision, text)
}

/// `_read`: one note for the editor, the frontmatter split off and shown as lines.
fn read(root: &Path, rel: &str) -> Result<Value, BrainError> {
    let path = sb::resolve(root, rel)?;
    if !path.is_file() || py::suffix(&path) != ".md" {
        return Err(BrainError::Value("no such markdown note".to_string()));
    }
    let text = py::read_text(&path, true)?;
    let (meta, body) = sb::split_frontmatter(&text);
    let rel = sb::rel(root, &path)?;
    let chars = py::char_len(&body);
    let clipped = chars > MAX_READ_CHARS;
    let front: Vec<String> = meta.iter().map(|(k, v)| format!("{k}: {v}")).collect();
    let modified = py::mtime(&path)?.trunc() as i64;
    let undo_id = sb::latest_edit(root, &rel, &text)?;
    Ok(json!({"ok": true, "path": rel, "title": sb::title_of(&text, &py::stem(&path)),
              "body": py::clip(&body, MAX_READ_CHARS), "front": front, "revision": sb::revision(&text),
              "modified": modified, "editable": !clipped && chars <= MAX_BODY_CHARS, "undo_id": undo_id}))
}

/// `_short`: `str(value or "").strip()[:limit]`.
fn short(value: Option<&Value>, limit: usize) -> String {
    let s = value.filter(|v| sb::truthy(v)).map(sb::value_str).unwrap_or_default();
    py::clip(py::strip(&s), limit).to_string()
}

/// second_brain's reasons, written for an agent, said to the owner.
pub fn owner_line(reason: &str) -> String {
    if reason.starts_with("note changed") {
        return CHANGED_LINE.to_string();
    }
    if reason.starts_with("no such") {
        return NOT_A_NOTE.to_string();
    }
    if reason.starts_with("nothing to restore") {
        return "There's nothing to restore.".to_string();
    }
    if reason.starts_with("not a vault folder") {
        return "That isn't one of your note folders.".to_string();
    }
    if reason.starts_with("use an undo_id") || reason.starts_with("invalid note history") {
        return "That change can't be undone any more.".to_string();
    }
    if reason.starts_with("path ") || reason.starts_with("hidden paths") {
        return NOT_A_NOTE.to_string();
    }
    format!("That didn't work: {reason}")
}

#[cfg(test)]
mod tests;
