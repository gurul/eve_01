//! codex_warm.py: one Codex agent started ahead of time; never a stale one handed out.
//! Port of bridge/tests/test_codex_warm.py.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::sync::Notify;

use super::*;

#[derive(Default)]
struct FakeState {
    warmed: AtomicBool,
    discarded: AtomicBool,
    alive: AtomicBool,
}

impl FakeState {
    fn warmed(&self) -> bool {
        self.warmed.load(Ordering::SeqCst)
    }
    fn discarded(&self) -> bool {
        self.discarded.load(Ordering::SeqCst)
    }
}

/// `Fake.made`: every agent the factory built, in order.
type Made = Arc<Mutex<Vec<Arc<FakeState>>>>;

struct Fake {
    st: Arc<FakeState>,
    on_event: OnEvent<()>,
    ask_user: AskUser,
    fail: bool,
    /// `Slow`: prewarm waits for this before it is warm.
    slow: Option<Arc<Notify>>,
}

impl Fake {
    fn new(made: &Made, on_event: OnEvent<()>, ask_user: AskUser, fail: bool, slow: Option<Arc<Notify>>) -> Self {
        let st = Arc::new(FakeState::default());
        st.alive.store(true, Ordering::SeqCst);
        made.lock().push(Arc::clone(&st));
        Fake { st, on_event, ask_user, fail, slow }
    }

    fn is(&self, other: &Arc<FakeState>) -> bool {
        Arc::ptr_eq(&self.st, other)
    }
}

impl WarmAgent for Fake {
    type Event = ();

    async fn prewarm(&mut self) -> anyhow::Result<()> {
        if let Some(slow) = &self.slow {
            slow.notified().await;
        }
        if self.fail {
            return Err(std::io::Error::other("no codex").into());
        }
        self.st.warmed.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn warm(&self, _max_age: f64) -> bool {
        self.st.warmed() && self.st.alive.load(Ordering::SeqCst)
    }

    async fn discard(&mut self) {
        self.st.discarded.store(true, Ordering::SeqCst);
    }

    fn set_callbacks(&mut self, on_event: OnEvent<()>, ask_user: AskUser) {
        self.on_event = on_event;
        self.ask_user = ask_user;
    }
}

fn pool_of(made: &Made, enabled: bool) -> WarmCodex<Fake> {
    let made = Arc::clone(made);
    WarmCodex::new(Arc::new(move |e, a| Fake::new(&made, e, a, false, None)), enabled, DEFAULT_MAX_AGE_SECS)
}

fn cb() -> (OnEvent<()>, AskUser) {
    (Arc::new(|_| {}), nobody_ask())
}

#[tokio::test]
async fn test_take_hands_the_warm_agent_this_tasks_callbacks() {
    let made = Made::default();
    let pool = pool_of(&made, true);
    pool.kick();
    pool.join_jobs().await;
    let (on_event, ask_user) = cb();
    let agent = pool.take(Arc::clone(&on_event), Arc::clone(&ask_user));
    assert!(agent.is(&made.lock()[0]));
    assert!(Arc::ptr_eq(&agent.on_event, &on_event) && Arc::ptr_eq(&agent.ask_user, &ask_user));
    let cold = pool.take(Arc::clone(&on_event), Arc::clone(&ask_user)); // none ready now: a new cold agent
    assert!(!cold.is(&agent.st) && !cold.st.warmed() && made.lock().len() == 2);
    assert!(Arc::ptr_eq(&cold.on_event, &on_event) && Arc::ptr_eq(&cold.ask_user, &ask_user));
}

#[tokio::test]
async fn test_a_dead_warm_agent_is_discarded_not_handed_out() {
    let made = Made::default();
    let pool = pool_of(&made, true);
    pool.kick();
    pool.join_jobs().await;
    made.lock()[0].alive.store(false, Ordering::SeqCst);
    let (e, a) = cb();
    let agent = pool.take(e, a);
    pool.join_jobs().await;
    let first = Arc::clone(&made.lock()[0]);
    assert!(!agent.is(&first) && first.discarded());
}

#[tokio::test]
async fn test_kick_is_single_and_disabled_does_nothing() {
    let made = Made::default();
    let pool = pool_of(&made, true);
    pool.kick();
    pool.kick();
    pool.join_jobs().await;
    pool.kick(); // one is ready: nothing more
    pool.join_jobs().await;
    assert_eq!(made.lock().len(), 1);
    let off = pool_of(&made, false);
    off.kick();
    off.join_jobs().await;
    assert_eq!(made.lock().len(), 1);
}

#[tokio::test]
async fn test_failed_warm_up_leaves_tasks_cold() {
    let made = Made::default();
    let m = Arc::clone(&made);
    let pool: WarmCodex<Fake> = WarmCodex::with_factory(move |e, a| Fake::new(&m, e, a, true, None));
    pool.kick();
    pool.join_jobs().await;
    assert!(!pool.is_ready());
    let (e, a) = cb();
    let agent = pool.take(e, a);
    assert!(!agent.st.warmed() && made.lock().len() == 2);
}

#[tokio::test]
async fn test_close_discards_the_ready_agent() {
    let made = Made::default();
    let pool = pool_of(&made, true);
    pool.kick();
    pool.join_jobs().await;
    pool.close().await;
    assert!(made.lock()[0].discarded() && !pool.is_ready());
}

/// refresh_loop kicked every 60 s regardless: once take() handed out the warm agent, the next refresh
/// started a second app-server during the task. A warm-up still in flight at take() is called off too.
#[tokio::test]
async fn test_the_refresh_loop_never_warms_a_second_app_server_while_a_task_runs() {
    let made = Made::default();
    let pool = pool_of(&made, true);
    pool.kick();
    pool.join_jobs().await;
    let (e, a) = cb();
    let task_agent = pool.take(e, a); // a task starts with the warm agent
    let lp = pool.clone();
    let looping = tokio::spawn(async move { lp.refresh_loop(Duration::from_secs(3600)).await });
    tokio::task::yield_now().await; // the loop's first pass: its kick, then its sleep
    pool.join_jobs().await;
    assert!(made.lock().len() == 1 && task_agent.is(&made.lock()[0]) && !pool.is_ready());
    pool.kick(); // the task ended (app_reflex's on_done)
    pool.join_jobs().await;
    assert_eq!(made.lock().len(), 2); // control: warming still works after it
    assert!(pool.is_ready());
    {
        let st = pool.shared.state.lock();
        assert!(st.ready.as_ref().unwrap().is(&made.lock()[1]));
    }

    let slow = Arc::new(Notify::new());
    let m = Arc::clone(&made);
    let s = Arc::clone(&slow);
    let pool2: WarmCodex<Fake> =
        WarmCodex::with_factory(move |e, a| Fake::new(&m, e, a, false, Some(Arc::clone(&s))));
    pool2.kick(); // warming, not ready yet
    tokio::task::yield_now().await;
    let (e, a) = cb();
    let _cold = pool2.take(e, a); // a task starts cold meanwhile
    slow.notify_waiters();
    pool2.join_jobs().await;
    assert!(!pool2.is_ready() && made.lock()[2].discarded());
    looping.abort();
    let _ = looping.await;
}

#[test]
fn test_configured() {
    let none = |_: &str| None::<String>;
    assert_eq!(configured_from(none), (true, DEFAULT_MAX_AGE_SECS));
    let env = |k: &str| match k {
        "EVE_CODEX_PREWARM" => Some("0".to_string()),
        "EVE_CODEX_WARM_SECS" => Some("5".to_string()),
        _ => None,
    };
    assert_eq!(configured_from(env), (false, 60.0));
}

fn with(prewarm: Option<&str>, secs: Option<&str>) -> (bool, f64) {
    let (p, s) = (prewarm.map(str::to_string), secs.map(str::to_string));
    configured_from(move |k| match k {
        "EVE_CODEX_PREWARM" => p.clone(),
        "EVE_CODEX_WARM_SECS" => s.clone(),
        _ => None,
    })
}

#[test]
fn configured_reads_the_switch_like_python() {
    for off in ["0", "false", "no", "off", " OFF ", "False", "No\n"] {
        assert!(!with(Some(off), None).0, "{off:?}");
    }
    for on in ["1", "", "yes", "true", "2", "nope"] {
        assert!(with(Some(on), None).0, "{on:?}");
    }
}

#[test]
fn configured_parses_the_age_like_python_float() {
    assert_eq!(with(None, Some("")).1, 900.0); // `or DEFAULT`
    assert_eq!(with(None, Some("abc")).1, 900.0); // ValueError → default
    assert_eq!(with(None, Some("  ")).1, 900.0);
    assert_eq!(with(None, Some(" 120 ")).1, 120.0);
    assert_eq!(with(None, Some("1_200")).1, 1200.0);
    assert_eq!(with(None, Some("1__200")).1, 900.0);
    assert_eq!(with(None, Some("_120")).1, 900.0);
    assert_eq!(with(None, Some("1.5e2")).1, 150.0);
    assert_eq!(with(None, Some("-5")).1, 60.0);
    assert_eq!(with(None, Some("nan")).1, 60.0); // max(60.0, nan) is 60.0
    assert_eq!(with(None, Some("Infinity")).1, f64::INFINITY);
}

#[tokio::test]
async fn nobody_answers_nothing() {
    assert_eq!(nobody("anything?".to_string()).await, "");
    assert_eq!((nobody_ask())("q".to_string()).await, "");
}

#[tokio::test]
async fn a_stale_ready_agent_is_replaced_by_the_refresh_loop() {
    let made = Made::default();
    let pool = pool_of(&made, true);
    pool.kick();
    pool.join_jobs().await;
    made.lock()[0].alive.store(false, Ordering::SeqCst);
    let lp = pool.clone();
    let looping = tokio::spawn(async move { lp.refresh_loop(Duration::from_secs(3600)).await });
    tokio::task::yield_now().await;
    pool.join_jobs().await;
    assert!(made.lock()[0].discarded() && made.lock().len() == 2 && pool.is_ready());
    looping.abort();
    let _ = looping.await;
    tokio::task::yield_now().await; // the loop's close() runs on drop
    pool.join_jobs().await;
    for _ in 0..10 {
        if made.lock()[1].discarded() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(made.lock()[1].discarded() && !pool.is_ready());
}

#[tokio::test]
async fn a_disabled_refresh_loop_returns_at_once() {
    let made = Made::default();
    let pool = pool_of(&made, false);
    pool.refresh_loop(Duration::from_secs(3600)).await;
    assert!(made.lock().is_empty());
}

#[tokio::test]
async fn take_calls_off_a_warm_up_that_never_finishes() {
    let made = Made::default();
    let m = Arc::clone(&made);
    let never = Arc::new(Notify::new());
    let pool: WarmCodex<Fake> =
        WarmCodex::with_factory(move |e, a| Fake::new(&m, e, a, false, Some(Arc::clone(&never))));
    pool.kick();
    tokio::task::yield_now().await; // the warm-up is inside prewarm, waiting forever
    let (e, a) = cb();
    let _cold = pool.take(e, a);
    pool.join_jobs().await; // returns only because the warm-up was called off
    assert!(made.lock()[0].discarded() && !made.lock()[0].warmed() && !pool.is_ready());
    pool.kick(); // the task ended: the next warm-up starts again
    tokio::task::yield_now().await;
    assert_eq!(made.lock().len(), 3);
    pool.close().await; // close calls the new one off too
    assert!(made.lock()[2].discarded());
}
