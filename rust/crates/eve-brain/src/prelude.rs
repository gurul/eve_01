//! What every module of the crate shares: the std, serde_json and sync names the skeleton's signatures use, the
//! environment type and its reader, and [`PyAny`], the skeleton's stand-in for a Python value whose Rust type the
//! filling unit chooses (a callable lent by the daemon, a duck-typed object).
//!
//! Architect-owned: no fill unit edits this file. A unit that replaces a `PyAny` in its own signatures with a real
//! type does so in its own files.

pub use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
pub use std::path::{Path, PathBuf};
pub use std::sync::{Arc, LazyLock};

pub use parking_lot::Mutex;
pub use regex::Regex;
pub use serde_json::{json, Map, Value};

/// A JSON object (Python's `dict[str, Any]`), key order kept (serde_json's `preserve_order`).
pub type Obj = Map<String, Value>;

/// The skeleton's stand-in for a Python value with no settled Rust type yet: a lent callable, a duck-typed object
/// from another crate. A filling unit replaces it, in its own files, with a concrete type or a trait object.
pub type PyAny = Arc<dyn std::any::Any + Send + Sync>;

/// The environment a call reads: `None` is the process's (Python's `os.environ`), `Some` a mapping lent by the
/// caller (Python's `environ` parameter, or a test's `monkeypatch.setenv`).
pub type Environ = HashMap<String, String>;

/// `env.get(key)`: from the lent mapping, else the process environment.
pub fn env_get(environ: Option<&Environ>, key: &str) -> Option<String> {
    match environ {
        Some(map) => map.get(key).cloned(),
        None => std::env::var(key).ok(),
    }
}

/// `env.get(key, default)`.
pub fn env_or(environ: Option<&Environ>, key: &str, default: &str) -> String {
    env_get(environ, key).unwrap_or_else(|| default.to_string())
}

/// Python's `Path.home()`: `$HOME` when set and not empty, else the account's home folder.
pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// Python's `Path(raw).expanduser()` for a leading `~` or `~/`.
pub fn expanduser(raw: &str) -> PathBuf {
    if raw == "~" {
        return home();
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return home().join(rest);
    }
    PathBuf::from(raw)
}

/// `time.monotonic()`: seconds on a clock that never goes back (offset so the first reading is not 0.0).
pub fn monotonic() -> f64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64() + 1000.0
}

/// `time.time()`: seconds since the epoch.
pub fn wall() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// A clock in seconds (`time.monotonic`, `time.time`, or a test's fake).
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

/// `asyncio.sleep` or a test's fake.
pub type Sleep = Arc<dyn Fn(f64) -> futures::future::BoxFuture<'static, ()> + Send + Sync>;

/// The brain's shared types (futures, tasks, the context, the daemon's callables and duck types).
pub use crate::seams::*;
