//! Eve Link's daemon half: a port of `link_chat.py`, `link_today.py`, `link_topics.py`, `updates.py` and
//! `updates_composio.py`.
//!
//! * [`link_chat`]: the app's chat door on `/api/link/*` (auth, a turn's SSE stream, the conversation read back from
//!   the transcript, the chief's cards for Work, uploads, controls, location, places, sessions, Muse).
//! * [`link_today`]: the home-screen widget's one read, `GET /api/link/today`.
//! * [`link_topics`]: Horizontal History, the chat cut into topics, their bookmarks and model titles.
//! * [`updates`]: the Updates pane's cards, made in the background from the owner's sources.
//! * [`updates_composio`]: Updates' Composio sources (Slack, Notion, Linear, the work Gmail and Calendar).
//!
//! What Python reached by duck typing on objects from other modules (the Telegram inlet, the chief, the Mini App
//! server, the direct Google apps, the Composio bridge, Work sessions) is a trait in [`seams`]. The production
//! objects of the crates this one depends on implement them there; the brain and the daemon implement the rest.

use std::collections::HashMap;

pub mod link_chat;
pub mod link_today;
pub mod link_topics;
pub mod seams;
pub mod updates;
pub mod updates_composio;

/// The environment a call reads: `None` is the process's (Python's `os.environ`), `Some` a mapping lent by the
/// caller (Python's `environ` parameter, or a test's `monkeypatch.setenv`).
pub type Env = HashMap<String, String>;

/// `env.get(key)`: from the lent mapping, else the process environment.
pub fn env_get(environ: Option<&Env>, key: &str) -> Option<String> {
    match environ {
        Some(map) => map.get(key).cloned(),
        None => std::env::var(key).ok(),
    }
}

/// Python's `Path(raw).expanduser()` for a leading `~` or `~/`.
pub fn expanduser(raw: &str) -> std::path::PathBuf {
    if raw == "~" || raw.starts_with("~/") {
        let home = std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(std::path::PathBuf::from)
            .or_else(dirs::home_dir);
        if let Some(home) = home {
            return if raw == "~" { home } else { home.join(&raw[2..]) };
        }
    }
    std::path::PathBuf::from(raw)
}

/// `time.monotonic()`: seconds on a clock that never goes back, as an f64 (Python's tests pass their own).
pub fn monotonic() -> f64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    // offset so the first reading is not 0.0 (Python's monotonic never is, and `_next_try = 0` means "no limit")
    START.get_or_init(Instant::now).elapsed().as_secs_f64() + 1000.0
}

/// A clock in seconds (`time.monotonic` or a test's fake).
pub type Mono = std::sync::Arc<dyn Fn() -> f64 + Send + Sync>;

/// The local wall clock, aware (`datetime.now().astimezone()` or a test's fake).
pub type Now = std::sync::Arc<dyn Fn() -> eve_transcripts::Dt + Send + Sync>;

/// `datetime.now().astimezone()`.
pub fn local_now() -> eve_transcripts::Dt {
    chrono::Local::now().fixed_offset()
}
