//! The phone's live location and the places history: a port of `phone_location.py` and `places.py`.
//!
//! `phone_location` keeps the Eve Link app's latest fix in one small JSON file and answers the brain's
//! `phone_location` tool; `places` keeps the history of fixes and iOS visits in one SQLite file and turns it into
//! words (visits, habits, digests, where things were left).
//!
//! Both read the same files the Python daemon writes, in the same formats: the fix file is Python's
//! `json.dump` output byte for byte, and the SQLite schema, queries and the `meta` table's JSON are unchanged.

pub mod phone_location;
pub mod places;
pub mod py;

use std::collections::HashMap;

/// The environment a call reads: `None` is the process's (Python's `os.environ`), `Some` a mapping lent by the
/// caller (Python's `environ` parameter).
pub type Env = HashMap<String, String>;

/// `env.get(key)`: from the lent mapping, else the process environment.
pub(crate) fn env_get(environ: Option<&Env>, key: &str) -> Option<String> {
    match environ {
        Some(map) => map.get(key).cloned(),
        None => std::env::var(key).ok(),
    }
}

/// Python's `Path(raw).expanduser()` for a leading `~` or `~/`.
pub(crate) fn expanduser(raw: &str) -> std::path::PathBuf {
    if raw == "~" || raw.starts_with("~/") {
        let home = std::env::var_os("HOME").filter(|h| !h.is_empty()).map(std::path::PathBuf::from).or_else(dirs::home_dir);
        if let Some(home) = home {
            return if raw == "~" { home } else { home.join(&raw[2..]) };
        }
    }
    std::path::PathBuf::from(raw)
}
