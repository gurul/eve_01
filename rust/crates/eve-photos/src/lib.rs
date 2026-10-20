//! Photos: the pictures eve keeps of things it finds cool.
//!
//! Port of `eve_bridge/photos.py`.
//!
//! The diary already turns a camera frame into a thought (diary.py). A few of
//! those views are worth more than a sentence, and for those eve asks the
//! board for one full-resolution frame (`{"cmd":"snap"}` → 320x240 at JPEG
//! quality 85, look.cpp) and keeps it.
//!
//! This module is only the shelf: where a photo goes, what it is called, how
//! many are kept, and how a kept photo is referenced from the diary. What is
//! worth photographing is decided in diary.py (the cool factor) — keeping the
//! two apart means the retention rules are testable without a model, a board
//! or a network, like the rest of the explorer.
//!
//! Layout under the notes directory (`EVE_NOTES_DIR`):
//!
//! ```text
//! photos/YYYY-MM-DD/HHMMSS-<record id>.jpg
//! ```
//!
//! so a day's pictures sit beside that day's diary file and the name sorts by
//! time. The path recorded in `memory.jsonl` and written into the day's
//! Markdown is always **relative to the notes directory**
//! (`photos/2026-09-06/141207-42.jpg`), so the directory can be moved or
//! mirrored (the macOS widget reads it out of the App Group container) without
//! rewriting a single record.
//!
//! In the Markdown the photo is an image line indented under its diary line:
//!
//! ```text
//! - 14:12 yaw=+20 pitch=40 — Someone left the good headphones on my desk.
//!   ![](photos/2026-09-06/141207-42.jpg)
//! ```
//!
//! The diary line itself is untouched, so every existing reader still parses it
//! (the Swift widget's NoteLine.parse and notes_widget.py both need a leading
//! `- HH:MM`); the image line has no time, so both skip it, and a Markdown
//! viewer shows the picture in place.
//!
//! Retention: photos are capped by count and by total bytes, oldest first, so
//! an unattended robot cannot fill a disk. Empty day directories are removed
//! with their last photo.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::NaiveDateTime;

pub const PHOTOS_DIR: &str = "photos";
pub const DEFAULT_MAX_PHOTOS: i64 = 300;
pub const DEFAULT_MAX_MB: f64 = 100.0;
pub const JPEG_MAGIC: &[u8] = b"\xff\xd8";

/// The two retention caps. `max_bytes` defaults to
/// `int(DEFAULT_MAX_MB * 1024 * 1024)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PhotoConfig {
    pub max_photos: i64,
    pub max_bytes: i64,
}

impl Default for PhotoConfig {
    fn default() -> Self {
        PhotoConfig {
            max_photos: DEFAULT_MAX_PHOTOS,
            max_bytes: (DEFAULT_MAX_MB * 1024.0 * 1024.0) as i64,
        }
    }
}

/// Python's `str.strip()`: Rust's Unicode whitespace plus the four ASCII
/// separators (`\x1c`-`\x1f`) Python's `isspace` also counts.
fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c))
}

/// Python's `float(str)` for an already-stripped string: Rust's parser plus
/// the underscore digit separators Python accepts (`"1_000"`). `None` where
/// Python raises `ValueError`.
fn py_float(raw: &str) -> Option<f64> {
    let s = py_strip(raw);
    if s.is_empty() {
        return None;
    }
    if s.contains('_') {
        // An underscore is only legal between two digits.
        let b = s.as_bytes();
        for (i, &c) in b.iter().enumerate() {
            if c == b'_' {
                let ok = i > 0
                    && i + 1 < b.len()
                    && b[i - 1].is_ascii_digit()
                    && b[i + 1].is_ascii_digit();
                if !ok {
                    return None;
                }
            }
        }
        return s.replace('_', "").parse::<f64>().ok();
    }
    s.parse::<f64>().ok()
}

/// Python's `repr()` of a str, for the `%r` in the warnings.
fn py_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python's `%g` for the default (a whole number of megabytes prints bare).
fn py_g(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e16 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

fn env_get<'a>(env: &'a HashMap<String, String>, name: &str) -> &'a str {
    env.get(name).map(String::as_str).unwrap_or("")
}

/// `_env_int`: blank → default; junk → warning and default; negative → 0.
fn env_int(name: &str, default: i64, environ: &HashMap<String, String>) -> i64 {
    let raw = py_strip(env_get(environ, name));
    if raw.is_empty() {
        return default;
    }
    match py_float(raw) {
        // int(float(raw)) truncates toward zero; NaN is a ValueError there.
        Some(f) if !f.is_nan() => (f as i64).max(0),
        _ => {
            tracing::warn!(
                "photos: {}={} is not a number; using {}",
                name,
                py_repr(raw),
                default
            );
            default
        }
    }
}

/// `configured(environ)`: `EVE_PHOTOS_MAX` (count) and `EVE_PHOTOS_MAX_MB`
/// (total size). `None` reads the process environment.
pub fn configured(environ: Option<&HashMap<String, String>>) -> PhotoConfig {
    match environ {
        Some(env) => configured_from(env),
        None => {
            let env: HashMap<String, String> = std::env::vars().collect();
            configured_from(&env)
        }
    }
}

/// `configured` over an explicit environment map.
pub fn configured_from(env: &HashMap<String, String>) -> PhotoConfig {
    let mb = py_strip(env_get(env, "EVE_PHOTOS_MAX_MB"));
    let max_mb = if mb.is_empty() {
        DEFAULT_MAX_MB
    } else {
        match py_float(mb) {
            Some(v) => v,
            None => {
                tracing::warn!(
                    "photos: EVE_PHOTOS_MAX_MB={} is not a number; using {}",
                    py_repr(mb),
                    py_g(DEFAULT_MAX_MB)
                );
                DEFAULT_MAX_MB
            }
        }
    };
    // max(0.0, nan) is 0.0 in Python and in Rust alike.
    let clamped = 0.0_f64.max(max_mb);
    PhotoConfig {
        max_photos: env_int("EVE_PHOTOS_MAX", DEFAULT_MAX_PHOTOS, env),
        max_bytes: (clamped * 1024.0 * 1024.0) as i64,
    }
}

pub fn photos_root(notes_dir: &Path) -> PathBuf {
    notes_dir.join(PHOTOS_DIR)
}

/// The notes-dir-relative path a photo taken now for this record gets.
pub fn relative_path(when: &NaiveDateTime, record_id: i64) -> String {
    format!(
        "{}/{}/{}-{}.jpg",
        PHOTOS_DIR,
        when.format("%Y-%m-%d"),
        when.format("%H%M%S"),
        record_id
    )
}

/// The Markdown image line that goes under the diary line.
pub fn photo_line(rel_path: &str) -> String {
    format!("  ![]({rel_path})")
}

/// Write one JPEG and prune. Returns its notes-dir-relative path, or `None`
/// when the bytes are not a JPEG, photos are switched off (max 0), or the
/// write failed — a photo is never worth losing a thought over.
pub fn save(
    notes_dir: &Path,
    when: &NaiveDateTime,
    record_id: i64,
    jpeg: &[u8],
    config: Option<PhotoConfig>,
) -> Option<String> {
    let cfg = config.unwrap_or_else(|| configured(None));
    if cfg.max_photos <= 0 || cfg.max_bytes <= 0 {
        return None;
    }
    if jpeg.is_empty() || !jpeg.starts_with(JPEG_MAGIC) {
        tracing::warn!("photos: not a JPEG ({} bytes) — not saved", jpeg.len());
        return None;
    }
    let rel = relative_path(when, record_id);
    let path = notes_dir.join(&rel);
    let write = || -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(photos_root(notes_dir), fs::Permissions::from_mode(0o700));
        }
        fs::write(&path, jpeg)
    };
    if let Err(e) = write() {
        tracing::warn!("photos: cannot write {}: {}", path.display(), e);
        return None;
    }
    prune(notes_dir, Some(cfg));
    Some(rel)
}

/// Python's `Path.suffix == ".jpg"`.
fn has_jpg_suffix(p: &Path) -> bool {
    p.extension().is_some_and(|e| e == "jpg")
}

fn sorted_children(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = match fs::read_dir(dir) {
        Ok(rd) => rd.filter_map(|e| e.ok().map(|e| e.path())).collect(),
        Err(_) => Vec::new(),
    };
    out.sort();
    out
}

/// Every kept photo, oldest first by name (the name sorts by date + time).
pub fn all_photos(notes_dir: &Path) -> Vec<PathBuf> {
    let root = photos_root(notes_dir);
    if !root.is_dir() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for day in sorted_children(&root).into_iter().filter(|p| p.is_dir()) {
        out.extend(sorted_children(&day).into_iter().filter(|f| has_jpg_suffix(f)));
    }
    out
}

/// Delete the oldest photos until both caps hold. Returns what went.
pub fn prune(notes_dir: &Path, config: Option<PhotoConfig>) -> Vec<PathBuf> {
    let cfg = config.unwrap_or_else(|| configured(None));
    let files = all_photos(notes_dir);
    let sizes: Vec<i64> = files
        .iter()
        .map(|f| fs::metadata(f).map(|m| m.len() as i64).unwrap_or(0))
        .collect();
    let mut total: i64 = sizes.iter().sum();
    let mut removed: Vec<PathBuf> = Vec::new();
    for (f, size) in files.iter().zip(sizes.iter()) {
        if (files.len() - removed.len()) as i64 <= cfg.max_photos && total <= cfg.max_bytes {
            break;
        }
        // Path.unlink: a file or a symlink, never a directory.
        if let Err(e) = fs::remove_file(f) {
            tracing::debug!("photos: cannot remove {}: {}", f.display(), e);
            continue;
        }
        total -= size;
        removed.push(f.clone());
        if let Some(parent) = f.parent() {
            rmdir_if_empty(parent);
        }
    }
    if !removed.is_empty() {
        tracing::info!(
            "photos: pruned {} (cap {} photos / {:.0} MB)",
            removed.len(),
            cfg.max_photos,
            cfg.max_bytes as f64 / 1024.0 / 1024.0
        );
    }
    removed
}

fn rmdir_if_empty(day_dir: &Path) {
    if let Ok(mut rd) = fs::read_dir(day_dir) {
        if rd.next().is_none() {
            let _ = fs::remove_dir(day_dir);
        }
    }
}

/// (count, total bytes) of the photos on the shelf.
pub fn usage(notes_dir: &Path) -> (usize, u64) {
    let files = all_photos(notes_dir);
    let total = files
        .iter()
        .filter_map(|f| fs::metadata(f).ok())
        .map(|m| m.len())
        .sum();
    (files.len(), total)
}

pub fn format_usage(notes_dir: &Path) -> String {
    let (n, total) = usage(notes_dir);
    format!(
        "{} photo{}, {:.1} MB",
        n,
        if n == 1 { "" } else { "s" },
        total as f64 / 1024.0 / 1024.0
    )
}

/// The newest `last` photos, newest first. `last == 0` means all of them; a
/// negative `last` follows Python's `files[-last:]` (drop the first `|last|`).
pub fn iter_recent(notes_dir: &Path, last: i64) -> Vec<PathBuf> {
    let files = all_photos(notes_dir);
    let n = files.len();
    let start = if last == 0 {
        0
    } else if last > 0 {
        n.saturating_sub(last as usize)
    } else {
        (last.unsigned_abs() as usize).min(n)
    };
    files[start..].iter().rev().cloned().collect()
}

#[cfg(test)]
mod tests;
