//! eve's web: the routed web search, the web reader body, and the watcher.
//!
//! A module-for-module port of `bridge/src/eve_bridge`:
//!
//! | Python            | Rust                |
//! |-------------------|---------------------|
//! | firecrawl.py      | [`firecrawl`]       |
//! | search_router.py  | [`search_router`]   |
//! | tinyfish.py       | [`tinyfish`]        |
//! | watch.py          | [`watch`] (a directory module, one file per section; the `eve-watch` bin is its `__main__`) |
//! | web_reader.py     | [`web_reader`]      |
//! | websearch.py      | [`websearch`]       |
//!
//! Python reaches the process through module globals: `os.environ`, `watch.http_request` (every TinyFish,
//! Firecrawl and watcher request), spend's ledger folder, Firecrawl's day count and Monid. Tests monkeypatch
//! those. Here they are one value, [`Ctx`], passed first to every function that touched one; [`Ctx::global`]
//! is the real process, and a test builds its own (`Ctx::global().clone().with_env(...)`), so tests run in
//! parallel threads without touching the process environment.
//!
//! Where Python returns a free-form dict (a search result, a tool reply), Rust returns `serde_json::Value`
//! with the same keys in the same order (`preserve_order`).

pub mod firecrawl;
pub mod search_router;
pub mod tinyfish;
pub mod watch;
pub mod web_reader;
pub mod websearch;

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

pub use watch::{FetchError, HttpOptions};

/// Python's `Mapping[str, str]` environment (`os.environ` or a test's dict). The same alias as
/// `eve_monid::Environ`.
pub type Environ = HashMap<String, String>;

/// A clock in seconds: `time.time` (epoch) or `time.perf_counter` (monotonic), whichever the Python lent.
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

/// `watch.openrouter`'s shape: one chat-completions body in, the reply's JSON object out. Raises FetchError.
pub type Ask = Arc<dyn Fn(&Value) -> Result<Value, FetchError> + Send + Sync>;

/// `watch.http_request`'s shape: one GET (POST with `opts.data`), (status, body text), FetchError otherwise.
/// Every TinyFish and Firecrawl call and every watcher read goes through one, so its guards hold.
pub trait Http: Send + Sync {
    fn request(&self, url: &str, opts: &HttpOptions) -> Result<(u16, String), FetchError>;
}

impl<F> Http for F
where
    F: Fn(&str, &HttpOptions) -> Result<(u16, String), FetchError> + Send + Sync,
{
    fn request(&self, url: &str, opts: &HttpOptions) -> Result<(u16, String), FetchError> {
        self(url, opts)
    }
}

/// The real [`Http`]: [`watch::net::http_request`] with the system resolver and address rules.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemHttp;

impl Http for SystemHttp {
    fn request(&self, url: &str, opts: &HttpOptions) -> Result<(u16, String), FetchError> {
        watch::net::http_request(url, opts)
    }
}

/// The process services Python reaches through module globals, as one value a test replaces.
#[derive(Clone)]
pub struct Ctx {
    /// `None`: the process environment (`os.environ`), read at each call as Python does.
    pub env: Option<Arc<Environ>>,
    /// `watch.http_request`.
    pub http: Arc<dyn Http>,
    /// spend's ledger (`spend.record`, `spend.record_chat_completion`).
    pub ledger: eve_spend::Ledger,
    /// firecrawl's `_CALLS` / `_LOADED` and its count file.
    pub firecrawl_calls: Arc<firecrawl::DayCount>,
    /// web_reader's `_TASKS`.
    pub reader_tasks: Arc<web_reader::TaskCount>,
    /// monid.py's module state (its runner, its count file).
    pub monid: Arc<eve_monid::Monid>,
}

static GLOBAL: LazyLock<Ctx> = LazyLock::new(|| Ctx {
    env: None,
    http: Arc::new(SystemHttp),
    ledger: eve_spend::Ledger::default(),
    firecrawl_calls: Arc::new(firecrawl::DayCount::default()),
    reader_tasks: Arc::new(web_reader::TaskCount::default()),
    monid: Arc::new(eve_monid::Monid::new()),
});

impl Ctx {
    /// The real process: its environment, the guarded HTTP, the configured spend folder, the shared counts.
    pub fn global() -> &'static Ctx {
        &GLOBAL
    }

    /// This context reading `env` instead of the process environment.
    pub fn with_env(mut self, env: Environ) -> Self {
        self.env = Some(Arc::new(env));
        self
    }

    /// This context sending its requests through `http` (a test's fake `watch.http_request`).
    pub fn with_http(mut self, http: Arc<dyn Http>) -> Self {
        self.http = http;
        self
    }

    /// This context writing spend lines into `ledger`.
    pub fn with_ledger(mut self, ledger: eve_spend::Ledger) -> Self {
        self.ledger = ledger;
        self
    }

    /// `env.get(name)`.
    pub fn var(&self, name: &str) -> Option<String> {
        match &self.env {
            Some(env) => env.get(name).cloned(),
            None => std::env::var(name).ok(),
        }
    }

    /// `env.get(name) or ""`.
    pub fn get(&self, name: &str) -> String {
        self.var(name).unwrap_or_default()
    }

    /// The environment as a map: the test's, or a snapshot of the process's (for the upstream crates that take
    /// `Option<&Environ>` or an `eve_jev::Env`).
    pub fn environ(&self) -> Environ {
        match &self.env {
            Some(env) => (**env).clone(),
            None => std::env::vars().collect(),
        }
    }
}

impl eve_jev::Env for Ctx {
    fn var(&self, name: &str) -> Option<String> {
        Ctx::var(self, name)
    }
}

static EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);

/// `time.perf_counter()`: seconds on a monotonic clock with an arbitrary origin.
pub fn perf_counter() -> f64 {
    EPOCH.elapsed().as_secs_f64()
}

/// `time.time()`: seconds since the epoch.
pub fn time_time() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// [`perf_counter`] as a [`Clock`].
pub fn perf_clock() -> Clock {
    Arc::new(perf_counter)
}

/// [`time_time`] as a [`Clock`].
pub fn wall_clock() -> Clock {
    Arc::new(time_time)
}
