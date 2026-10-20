//! Reactions on the owner's messages in the Eve Link app: Telegram's receipts, in the app (docs/app-chat.md).
//!
//! Owner, 2026-10-09: "can you build in reactions for when it get image or takes not or sees text and things like
//! that like telegram". In Telegram eve puts a reaction on the owner's own message instead of a one-line
//! acknowledgement (`TelegramInlet._receipt`): 👀 an image is being read, ✍ a note was kept, 🏆 a fact was starred,
//! 👍 it was typed to Claude or Codex. An app turn had no Telegram message to put it on (message id 0), so the
//! receipt fell back to a line.
//!
//! This crate gives an app turn's owner message an id a reaction can land on, and keeps the reactions:
//!
//! * [`Reactions::mint`]: the int the inlet uses as that message's `Inbound.message_id`. Always negative, so it can
//!   never be a Telegram message id (those are positive) and is never sent to Telegram. `key` is the app turn's id
//!   (link_chat: the app's `client_id` when it is a plain id, else a random one), which the owner's transcript line
//!   keeps as `mid`; `/api/link/messages` gives that row `rid` and its `reaction`.
//! * [`Reactions::set`]: Telegram's semantics exactly. A message holds at most one reaction from eve, a new one
//!   replaces the old, and "" takes it off. An id this store did not mint (or has since let go of) is
//!   [`SetError::NoSuchMessage`], and the inlet then says the receipt's line instead, as it does when Telegram
//!   refuses a reaction.
//! * [`Reactions::version`]: changes on every set, so a long poll can tell the app has an old picture. It starts at
//!   the wall clock in milliseconds, so a restart never reuses a version an app is still holding.
//!
//! Kept in `path` (JSON, 0600, the newest [`MAX_KEPT`] reactions) when given, so history shows them after a restart;
//! in memory only without one. A [`TRANSIENT`] reaction (👀, "reading this now") is never kept in the file: after a
//! crash nothing could take it off. Every listener is called after a change (link_chat wakes its long polls), and
//! one that fails is ignored. Logs carry the emoji and counts, never what was said.

use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use serde_json::Value;
use unicode_general_category::{get_general_category, GeneralCategory};

/// Reactions kept (and sent to the app on every poll); the oldest go first.
pub const MAX_KEPT: usize = 200;
/// Message ids remembered; the oldest go first.
pub const MAX_MINTED: usize = 512;
/// UTF-16 units: one emoji, a ZWJ sequence with modifiers, fits in this.
pub const MAX_EMOJI_CHARS: usize = 16;
/// The pattern a message key must match (Python `KEY_RE`); see [`key_ok`].
pub const KEY_RE: &str = r"^[A-Za-z0-9_-]{1,64}$";
pub const FILE_NAME: &str = "link-reactions.json";
/// Reactions that mean "eve is working on this now" (👀: an image is being read). They live in memory only: after a
/// crash or a forced kill the turn that would take one off is gone, and the ids that could reach it are not kept, so
/// a saved one would stay on the message for good (review, 2026-10-09). Never written to the file, dropped when read.
pub const TRANSIENT: &[&str] = &["\u{1F440}"];

/// Whether `emoji` is one of the [`TRANSIENT`] reactions.
pub fn is_transient(emoji: &str) -> bool {
    TRANSIENT.contains(&emoji)
}

/// `KEY_RE.match(key)`: 1 to 64 of `[A-Za-z0-9_-]`. Python's `$` also matches just before a final "\n", so a key
/// with one trailing newline passes there; it passes here too, so both daemons accept the same keys.
pub fn key_ok(key: &str) -> bool {
    let body = key.strip_suffix('\n').unwrap_or(key);
    (1..=64).contains(&body.len()) && body.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// An emoji's characters: symbols (So), skin-tone modifiers (Sk), the emoji variation selector (Mn), a keycap (Me),
/// the zero-width joiner (Cf). Letters, digits, punctuation, spaces and control characters are not among them.
fn emoji_char(ch: char) -> bool {
    (ch as u32) >= 0x2000
        && matches!(
            get_general_category(ch),
            GeneralCategory::OtherSymbol
                | GeneralCategory::ModifierSymbol
                | GeneralCategory::NonspacingMark
                | GeneralCategory::EnclosingMark
                | GeneralCategory::Format
        )
}

/// `emoji` when it is a short emoji string, else "". Anything ASCII, a space or a control character makes it "": a
/// reaction is never words (and never markup) wherever it is drawn.
pub fn clean(emoji: &str) -> &str {
    // counted in UTF-16 units, as the app's page counts it (reactions.ts MAX_REACTION_CHARS)
    let units = emoji.encode_utf16().count();
    if !(1..=MAX_EMOJI_CHARS).contains(&units) {
        return "";
    }
    if emoji.chars().all(emoji_char) {
        emoji
    } else {
        ""
    }
}

/// [`clean`] for a free-form value (Python's `clean(emoji: Any)`): anything but a string is "".
pub fn clean_value(emoji: &Value) -> &str {
    match emoji {
        Value::String(s) => clean(s),
        _ => "",
    }
}

/// Python raised `ValueError("bad message key")` from `mint`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("bad message key")]
pub struct BadKey;

/// Why [`Reactions::set`] refused: Python's `ValueError` and `KeyError`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SetError {
    /// `ValueError("not an emoji")`
    #[error("not an emoji")]
    NotAnEmoji,
    /// `KeyError("no such app message")`
    #[error("no such app message")]
    NoSuchMessage,
}

/// A listener: called with the message key after every change. An `Err` (or a panic) is logged and ignored.
pub type Listener = Arc<dyn Fn(&str) -> anyhow::Result<()> + Send + Sync>;

struct Inner {
    path: Option<PathBuf>,
    /// key -> emoji, oldest set first
    by_key: Vec<(String, String)>,
    /// message id -> key, oldest first
    minted: VecDeque<(i64, String)>,
    next: i64,
    version: i64,
}

impl Inner {
    fn find(&self, key: &str) -> Option<usize> {
        self.by_key.iter().position(|(k, _)| k == key)
    }

    /// Write the file (under the lock). Atomic: a crash leaves the old file or the new one, never half.
    fn save(&self) {
        let Some(path) = &self.path else { return };
        let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
        name.push(".tmp");
        let tmp = path.with_file_name(name);
        let result = (|| -> io::Result<()> {
            // std opens with O_CLOEXEC already
            let mut f = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&tmp)?;
            let kept: Vec<&(String, String)> = self.by_key.iter().filter(|(_, e)| !is_transient(e)).collect();
            f.write_all(dump(&kept).as_bytes())?;
            drop(f);
            fs::rename(&tmp, path)
        })();
        if let Err(e) = result {
            tracing::warn!("link reactions: not saved ({}); kept in memory", os_error_name(&e));
        }
    }
}

/// `json.dump(d, f, ensure_ascii=False)`: Python's default separators (", " and ": "), characters unescaped.
fn dump(pairs: &[&(String, String)]) -> String {
    let body: Vec<String> = pairs
        .iter()
        .map(|(k, e)| format!("{}: {}", Value::from(k.as_str()), Value::from(e.as_str())))
        .collect();
    format!("{{{}}}", body.join(", "))
}

/// The Python exception class an `OSError` with this errno would be, for the log line.
fn os_error_name(e: &io::Error) -> &'static str {
    match e.raw_os_error() {
        Some(libc::ENOENT) => "FileNotFoundError",
        Some(libc::EACCES) | Some(libc::EPERM) => "PermissionError",
        Some(libc::EISDIR) => "IsADirectoryError",
        Some(libc::ENOTDIR) => "NotADirectoryError",
        Some(libc::EEXIST) => "FileExistsError",
        _ => "OSError",
    }
}

/// Read the file: what it holds that is still good (a plain key, a clean emoji, never a transient one).
fn load(path: &Path) -> Vec<(String, String)> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => return warn_unread(path, os_error_name(&e)),
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return warn_unread(path, "UnicodeDecodeError");
    };
    let raw: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return warn_unread(path, "JSONDecodeError"),
    };
    let Value::Object(map) = raw else { return Vec::new() };
    map.iter()
        .filter(|(key, emoji)| {
            let e = clean_value(emoji);
            key_ok(key) && !e.is_empty() && !is_transient(e)
        })
        .map(|(key, emoji)| (key.clone(), clean_value(emoji).to_string()))
        .collect()
}

fn warn_unread(path: &Path, kind: &str) -> Vec<(String, String)> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    tracing::warn!("link reactions: {} not read ({}); starting empty", name, kind);
    Vec::new()
}

fn wall_clock() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// The app's reactions, by message key. Safe to call from any thread (share it in an `Arc`).
pub struct Reactions {
    inner: Mutex<Inner>,
    listeners: Mutex<Vec<Listener>>,
}

impl Default for Reactions {
    fn default() -> Self {
        Self::new(None)
    }
}

impl Reactions {
    /// `Reactions(path)`: kept in `path` when given, in memory only without one; the version starts at the wall
    /// clock in milliseconds.
    pub fn new(path: Option<&Path>) -> Self {
        Self::with_clock(path, wall_clock)
    }

    /// `Reactions(path, clock=...)`: `clock` gives seconds since the epoch, read once for the starting version.
    pub fn with_clock(path: Option<&Path>, clock: impl Fn() -> f64) -> Self {
        let r = Self {
            inner: Mutex::new(Inner {
                path: None,
                by_key: Vec::new(),
                minted: VecDeque::new(),
                next: -1,
                version: (clock() * 1000.0) as i64,
            }),
            listeners: Mutex::new(Vec::new()),
        };
        if let Some(p) = path {
            r.attach(p);
        }
        r
    }

    // -- where they are kept --

    /// Keep the reactions in `path` from now on, loading what it holds (once; a second call is ignored).
    pub fn attach(&self, path: &Path) {
        let mut inner = self.inner.lock();
        if inner.path.is_some() {
            return;
        }
        inner.path = Some(path.to_path_buf());
        let mut loaded = load(path);
        // a reaction set before the file was found wins (dict.update: a key already loaded keeps its place)
        for (k, e) in std::mem::take(&mut inner.by_key) {
            match loaded.iter_mut().find(|(lk, _)| *lk == k) {
                Some(slot) => slot.1 = e,
                None => loaded.push((k, e)),
            }
        }
        let start = loaded.len().saturating_sub(MAX_KEPT);
        inner.by_key = loaded.split_off(start);
    }

    /// The file the reactions are kept in, once attached.
    pub fn path(&self) -> Option<PathBuf> {
        self.inner.lock().path.clone()
    }

    // -- ids --

    /// The negative message id for the app message `key` (the same id for the same key while remembered).
    pub fn mint(&self, key: &str) -> Result<i64, BadKey> {
        if !key_ok(key) {
            return Err(BadKey);
        }
        let mut inner = self.inner.lock();
        if let Some((mid, _)) = inner.minted.iter().find(|(_, k)| k == key) {
            return Ok(*mid);
        }
        let mid = inner.next;
        inner.next -= 1;
        inner.minted.push_back((mid, key.to_string()));
        while inner.minted.len() > MAX_MINTED {
            inner.minted.pop_front();
        }
        Ok(mid)
    }

    /// An id this kind of store mints (negative), never a Telegram message's.
    pub fn is_app(message_id: i64) -> bool {
        message_id < 0
    }

    // -- reactions --

    /// Put `emoji` on the message (replacing eve's reaction there), or take it off with "".
    /// [`SetError::NoSuchMessage`]: not a message this store knows; [`SetError::NotAnEmoji`]: not an emoji.
    pub fn set(&self, message_id: i64, emoji: &str) -> Result<(), SetError> {
        if !emoji.is_empty() && clean(emoji).is_empty() {
            return Err(SetError::NotAnEmoji);
        }
        let key = {
            let mut inner = self.inner.lock();
            let key = inner
                .minted
                .iter()
                .find(|(mid, _)| *mid == message_id)
                .map(|(_, k)| k.clone())
                .ok_or(SetError::NoSuchMessage)?;
            let had = match inner.find(&key) {
                Some(i) => inner.by_key.remove(i).1,
                None => String::new(),
            };
            if !emoji.is_empty() {
                inner.by_key.push((key.clone(), emoji.to_string())); // newest last
                while inner.by_key.len() > MAX_KEPT {
                    inner.by_key.remove(0);
                }
            }
            if had == emoji {
                return Ok(()); // no change: nobody to wake
            }
            inner.version += 1;
            inner.save();
            key
        };
        tracing::info!("link reactions: {}", if emoji.is_empty() { "cleared" } else { emoji });
        let listeners: Vec<Listener> = self.listeners.lock().clone();
        for listener in listeners {
            // a listener never costs the reaction
            let failed = match catch_unwind(AssertUnwindSafe(|| listener(&key))) {
                Ok(Ok(())) => None,
                Ok(Err(_)) => Some("Error"),
                Err(_) => Some("panic"),
            };
            if let Some(kind) = failed {
                tracing::warn!("link reactions: a listener failed ({})", kind);
            }
        }
        Ok(())
    }

    /// The reaction on the message `key`, or "".
    pub fn get(&self, key: &str) -> String {
        let inner = self.inner.lock();
        inner.find(key).map(|i| inner.by_key[i].1.clone()).unwrap_or_default()
    }

    /// Every reaction kept, by message key, oldest set first (what `/api/link/messages` sends as `reactions`).
    pub fn snapshot(&self) -> serde_json::Map<String, Value> {
        self.inner.lock().by_key.iter().map(|(k, e)| (k.clone(), Value::from(e.as_str()))).collect()
    }

    /// Changes on every set; starts at the wall clock in milliseconds.
    pub fn version(&self) -> i64 {
        self.inner.lock().version
    }

    /// `listeners.append(fn)`: `fn` is called with the key after every change.
    pub fn add_listener(&self, listener: Listener) {
        self.listeners.lock().push(listener);
    }

    /// `fn in listeners`: by identity (link_chat adds its waker only once).
    pub fn has_listener(&self, listener: &Listener) -> bool {
        self.listeners.lock().iter().any(|l| Arc::ptr_eq(l, listener))
    }

    /// `listeners.remove(fn)`: by identity; false when it was not there.
    pub fn remove_listener(&self, listener: &Listener) -> bool {
        let mut ls = self.listeners.lock();
        match ls.iter().position(|l| Arc::ptr_eq(l, listener)) {
            Some(i) => {
                ls.remove(i);
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests;
