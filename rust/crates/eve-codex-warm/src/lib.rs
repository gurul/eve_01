//! Keep one Codex computer agent warm, so a hard task's handoff starts the turn at once.
//!
//! Port of `eve_bridge/codex_warm.py`.
//!
//! A cold `CodexComputerAgent.run` spends 4.8-7.0 s (measured 2026-09-23) before the goal is used at
//! all: the app-server process, `initialize`, an ephemeral thread, and the check that Computer Use's
//! `cua_repl` tool is there. None of that depends on the goal, so [`WarmCodex`] does it ahead of time
//! ([`WarmAgent::prewarm`]) and hands the ready agent to the next task that needs Codex.
//!
//! * One warm agent at most, and it is single-use: the task closes it, as a cold one is closed.
//! * The next one is warmed after a task ends ([`WarmCodex::kick`]), not while it runs: two app-servers
//!   never share the desktop during a task.
//! * A warm agent older than `EVE_CODEX_WARM_SECS` (default 900), or whose app-server died, is
//!   thrown away and replaced by the refresh loop; [`WarmCodex::take`] never hands out a stale one —
//!   the task then starts cold, exactly as before this module.
//! * `EVE_CODEX_PREWARM=0` turns it off. A warm-up that fails is logged and retried on the next
//!   refresh; it never touches a task.
//!
//! The agent type is a parameter ([`WarmAgent`]), as Python's `WarmCodex` takes the agent class as
//! its factory: the daemon passes the Codex computer agent, the tests pass a fake.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use parking_lot::Mutex;
use tokio::sync::watch;
use tokio::task::JoinHandle;

pub const DEFAULT_MAX_AGE_SECS: f64 = 900.0;
pub const REFRESH_SECS: f64 = 60.0;

/// The `on_event` callback an agent reports its events to.
pub type OnEvent<E> = Arc<dyn Fn(E) + Send + Sync>;

/// The `ask_user` callback: a question in, the owner's answer out.
pub type AskUser = Arc<dyn Fn(String) -> BoxFuture<'static, String> + Send + Sync>;

/// What `WarmCodex` needs from an agent: the goal-free start, the freshness check, the close, and
/// the two callbacks a task hands over at `take`.
pub trait WarmAgent: Send + 'static {
    /// The event type `on_event` receives.
    type Event: 'static;

    /// Do the goal-free part of `run` ahead of time. On failure the agent has closed what it started.
    fn prewarm(&mut self) -> impl Future<Output = anyhow::Result<()>> + Send;

    /// Prewarmed, younger than `max_age` seconds, and its app-server still alive.
    fn warm(&self, max_age: f64) -> bool;

    /// Close a prewarmed agent that will not run.
    fn discard(&mut self) -> impl Future<Output = ()> + Send;

    /// Give the agent this task's callbacks (Python assigns `agent.on_event, agent.ask_user`).
    fn set_callbacks(&mut self, on_event: OnEvent<Self::Event>, ask_user: AskUser);

    /// The short name logged when a warm-up fails. Python logs `type(e).__name__` and nothing else, so
    /// no detail of the failure reaches the log; an agent overrides this to name its error kind.
    fn failure_name(err: &anyhow::Error) -> String {
        let _ = err;
        "Error".to_string()
    }
}

/// Builds an agent with the given callbacks (Python's `factory(on_event=..., ask_user=...)`).
pub type Factory<A> = Arc<dyn Fn(OnEvent<<A as WarmAgent>::Event>, AskUser) -> A + Send + Sync>;

/// `configured(environ)`: whether prewarming is on, and the max age in seconds, from the process
/// environment.
pub fn configured() -> (bool, f64) {
    configured_from(|k| std::env::var(k).ok())
}

/// `configured(environ)` with an explicit environment lookup.
pub fn configured_from(env: impl Fn(&str) -> Option<String>) -> (bool, f64) {
    let flag = env("EVE_CODEX_PREWARM").unwrap_or_else(|| "1".to_string());
    let on = !matches!(flag.trim().to_lowercase().as_str(), "0" | "false" | "no" | "off");
    let raw = env("EVE_CODEX_WARM_SECS").unwrap_or_default();
    // float(env.get(...) or DEFAULT): an empty value is the default, a bad one falls back to it too
    let age = if raw.is_empty() {
        DEFAULT_MAX_AGE_SECS
    } else {
        python_float(&raw).unwrap_or(DEFAULT_MAX_AGE_SECS)
    };
    // max(60.0, age): Python keeps the first argument unless the second is greater (so NaN gives 60)
    (on, if age > 60.0 { age } else { 60.0 })
}

/// Python's `float(str)`: surrounding whitespace ignored, `_` allowed between digits, `inf`/`nan`
/// in any case, with an optional sign.
fn python_float(s: &str) -> Option<f64> {
    let t = s.trim();
    let b = t.as_bytes();
    let mut clean = String::with_capacity(t.len());
    for (i, ch) in t.char_indices() {
        if ch == '_' {
            let before = i.checked_sub(1).map(|j| b[j].is_ascii_digit()).unwrap_or(false);
            let after = b.get(i + 1).map(|c| c.is_ascii_digit()).unwrap_or(false);
            if !(before && after) {
                return None;
            }
            continue;
        }
        clean.push(ch);
    }
    if clean.is_empty() {
        return None;
    }
    clean.parse::<f64>().ok()
}

/// `_nobody`: a warm agent has no task; it asks nothing until take() gives it one.
pub async fn nobody(question: String) -> String {
    let _ = question;
    String::new()
}

/// [`nobody`] as an [`AskUser`] callback.
pub fn nobody_ask() -> AskUser {
    Arc::new(|q| Box::pin(nobody(q)))
}

struct State<A> {
    ready: Option<A>,
    /// The warm-up in flight: sending `true` calls it off. It is done once its receivers are gone,
    /// which is when its task has finished.
    warming: Option<watch::Sender<bool>>,
    jobs: Vec<JoinHandle<()>>,
    /// take() handed an agent to a task and kick() has not said it ended: the refresh loop warms nothing,
    /// or a second app-server would share the desktop with the task
    in_use: bool,
}

struct Shared<A: WarmAgent> {
    factory: Factory<A>,
    enabled: bool,
    max_age: f64,
    state: Mutex<State<A>>,
}

/// One Codex agent started ahead of time; never a stale one handed out. Cloning shares the pool.
pub struct WarmCodex<A: WarmAgent> {
    shared: Arc<Shared<A>>,
}

impl<A: WarmAgent> Clone for WarmCodex<A> {
    fn clone(&self) -> Self {
        Self { shared: Arc::clone(&self.shared) }
    }
}

impl<A: WarmAgent> WarmCodex<A> {
    /// `WarmCodex(factory, enabled=True, max_age=DEFAULT_MAX_AGE_SECS)`.
    pub fn new(factory: Factory<A>, enabled: bool, max_age: f64) -> Self {
        Self {
            shared: Arc::new(Shared {
                factory,
                enabled,
                max_age,
                state: Mutex::new(State { ready: None, warming: None, jobs: Vec::new(), in_use: false }),
            }),
        }
    }

    /// A pool with Python's defaults: enabled, `DEFAULT_MAX_AGE_SECS`.
    pub fn with_factory(factory: impl Fn(OnEvent<A::Event>, AskUser) -> A + Send + Sync + 'static) -> Self {
        Self::new(Arc::new(factory), true, DEFAULT_MAX_AGE_SECS)
    }

    pub fn enabled(&self) -> bool {
        self.shared.enabled
    }

    pub fn max_age(&self) -> f64 {
        self.shared.max_age
    }

    /// Whether a warm agent is waiting for the next task.
    pub fn is_ready(&self) -> bool {
        self.shared.state.lock().ready.is_some()
    }

    /// `_spawn`: run a job the pool keeps track of, so close() can wait for it.
    fn spawn(st: &mut State<A>, fut: impl Future<Output = ()> + Send + 'static) {
        st.jobs.retain(|j| !j.is_finished());
        match tokio::runtime::Handle::try_current() {
            Ok(h) => st.jobs.push(h.spawn(fut)),
            Err(_) => tracing::warn!("codex-warm: no tokio runtime to run a job on; it was dropped"),
        }
    }

    /// A task ended (app_reflex's on_done), or the daemon starts: warm one now, unless one is ready or
    /// warming. Safe to call from any callback on the runtime.
    pub fn kick(&self) {
        self.shared.state.lock().in_use = false;
        self.kick_inner();
    }

    /// `_kick`.
    fn kick_inner(&self) {
        let mut st = self.shared.state.lock();
        let warming = st.warming.as_ref().is_some_and(|w| !w.is_closed());
        if !self.shared.enabled || st.in_use || st.ready.is_some() || warming {
            return;
        }
        let (tx, rx) = watch::channel(false);
        st.warming = Some(tx);
        let pool = self.clone();
        Self::spawn(&mut st, async move { pool.warm_up(rx).await });
    }

    /// `_warm`.
    async fn warm_up(&self, rx: watch::Receiver<bool>) {
        let mut agent = (self.shared.factory)(Arc::new(|_| {}), nobody_ask());
        let mut cancel = rx.clone();
        let called_off = async move {
            if cancel.wait_for(|c| *c).await.is_err() {
                std::future::pending::<()>().await;
            }
        };
        let outcome = tokio::select! {
            biased;
            _ = called_off => None,
            r = agent.prewarm() => Some(r),
        };
        match outcome {
            None => agent.discard().await,
            Some(Err(e)) => {
                // Codex missing or down: tasks start cold, as before
                tracing::warn!(
                    "codex-warm: warm-up failed ({}); tasks start cold until the next refresh",
                    A::failure_name(&e)
                );
            }
            Some(Ok(())) => {
                let late = {
                    let mut st = self.shared.state.lock();
                    if *rx.borrow() {
                        // take() or close() called it off as the warm-up finished: it is not handed out
                        Some(agent)
                    } else {
                        st.ready = Some(agent);
                        None
                    }
                };
                match late {
                    Some(mut agent) => agent.discard().await,
                    None => tracing::info!("codex-warm: an agent is ready"),
                }
            }
        }
    }

    /// The warm agent, given this task's callbacks; or a new cold one when none is ready. Nothing is warmed
    /// again until kick() says the task ended, and a warm-up still in flight is called off.
    pub fn take(&self, on_event: OnEvent<A::Event>, ask_user: AskUser) -> A {
        let agent = {
            let mut st = self.shared.state.lock();
            st.in_use = true;
            if let Some(w) = st.warming.as_ref() {
                if !w.is_closed() {
                    w.send_replace(true); // warm_up discards what it started
                }
            }
            st.ready.take()
        };
        if let Some(mut agent) = agent {
            if agent.warm(self.shared.max_age) {
                agent.set_callbacks(on_event, ask_user);
                return agent;
            }
            let mut st = self.shared.state.lock();
            Self::spawn(&mut st, async move { agent.discard().await });
        }
        (self.shared.factory)(on_event, ask_user)
    }

    /// Warm one at start, and replace it whenever it goes stale or its app-server dies. Runs until its
    /// task is dropped (cancelled); then the pool is closed, as Python's `finally: await self.close()`.
    pub async fn refresh_loop(&self, every: Duration) {
        if !self.shared.enabled {
            return;
        }
        let _close = CloseOnDrop(self.clone());
        loop {
            let stale = {
                let mut st = self.shared.state.lock();
                if st.ready.as_ref().is_some_and(|r| !r.warm(self.shared.max_age)) {
                    st.ready.take()
                } else {
                    None
                }
            };
            if let Some(mut ready) = stale {
                ready.discard().await;
            }
            self.kick_inner(); // never while a task holds the one it took
            tokio::time::sleep(every).await;
        }
    }

    /// `refresh_loop()` with Python's default period, `REFRESH_SECS`.
    pub async fn refresh_loop_default(&self) {
        self.refresh_loop(Duration::from_secs_f64(REFRESH_SECS)).await
    }

    /// Call off a warm-up in flight, discard the ready agent, and wait for every job.
    pub async fn close(&self) {
        let (ready, jobs) = {
            let mut st = self.shared.state.lock();
            if let Some(w) = st.warming.as_ref() {
                w.send_replace(true);
            }
            (st.ready.take(), std::mem::take(&mut st.jobs))
        };
        if let Some(mut ready) = ready {
            ready.discard().await;
        }
        for job in jobs {
            let _ = job.await;
        }
    }

    /// Wait for the jobs started so far (warm-ups and discards), as `asyncio.gather(*pool._jobs)`.
    pub async fn join_jobs(&self) {
        let jobs = std::mem::take(&mut self.shared.state.lock().jobs);
        for job in jobs {
            let _ = job.await;
        }
    }
}

/// Runs `close()` when `refresh_loop` stops, including when its task is cancelled.
struct CloseOnDrop<A: WarmAgent>(WarmCodex<A>);

impl<A: WarmAgent> Drop for CloseOnDrop<A> {
    fn drop(&mut self) {
        let pool = self.0.clone();
        if let Ok(h) = tokio::runtime::Handle::try_current() {
            h.spawn(async move { pool.close().await });
        }
    }
}

#[cfg(test)]
mod tests;
