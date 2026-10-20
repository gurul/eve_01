//! Port of `bridge/tests/test_agentcard_tools.py`: offline approval lifecycle with
//! real workers, explicit barriers and no HTTP client.

use super::*;

use std::sync::Condvar;
use std::thread::ThreadId;
use std::time::Duration;

use tokio::sync::oneshot;

const OWNER: i64 = 101;
const OTHER_OWNER: i64 = 202;
const HASH: &str = "0123456789abcdef";
const OTHER_HASH: &str = "fedcba9876543210";

fn args() -> Value {
    json!({"conversation_id": "conversation-1", "cart_hashes": [HASH]})
}

/// `threading.Event`.
#[derive(Default)]
struct Event {
    flag: std::sync::Mutex<bool>,
    cv: Condvar,
}

impl Event {
    fn set(&self) {
        *self.flag.lock().unwrap() = true;
        self.cv.notify_all();
    }

    fn clear(&self) {
        *self.flag.lock().unwrap() = false;
    }

    fn wait(&self, timeout: Duration) -> bool {
        let guard = self.flag.lock().unwrap();
        let (guard, _) = self.cv.wait_timeout_while(guard, timeout, |set| !*set).unwrap();
        *guard
    }
}

#[derive(Debug, PartialEq, Eq)]
struct SnapshotData {
    conversation: String,
    hashes: Vec<String>,
}

type Snap = Arc<SnapshotData>;

fn snap(conversation: &str, hashes: &[&str]) -> Snap {
    Arc::new(SnapshotData { conversation: conversation.into(), hashes: hashes.iter().map(|h| h.to_string()).collect() })
}

/// Confirmation checks Stop before submitting, then retains any real outcome.
#[derive(Default)]
struct TestService {
    handled: Mutex<Vec<(String, Value, Option<ThreadId>)>>,
    prepared: Mutex<Vec<Snap>>,
    confirmed: Mutex<Vec<(Snap, Arc<AtomicBool>)>>,
    submitted: Mutex<Vec<Snap>>,
    prepare_entered: Event,
    confirm_entered: Event,
    submit_entered: Event,
    prepare_release: Event,
    dispatch_release: Event,
    response_release: Event,
    /// `prepare_result`: the dict, and whether it carries a snapshot.
    prepare_result: Mutex<Option<(Value, bool)>>,
    confirm_error: Mutex<Option<String>>,
    /// The `Broken` subclass: unusable returns from `handle` and `confirm`.
    broken: bool,
}

impl TestService {
    fn new() -> Arc<Self> {
        Self::build(false)
    }

    fn build(broken: bool) -> Arc<Self> {
        let service = TestService { broken, ..Default::default() };
        service.release();
        Arc::new(service)
    }

    fn release(&self) {
        for event in [&self.prepare_release, &self.dispatch_release, &self.response_release] {
            event.set();
        }
    }

    fn prepared(&self) -> usize {
        self.prepared.lock().len()
    }

    fn confirmed(&self) -> usize {
        self.confirmed.lock().len()
    }

    fn submitted(&self) -> usize {
        self.submitted.lock().len()
    }
}

impl Service for TestService {
    type Snapshot = Snap;

    fn handle(&self, name: &str, args: Value) -> anyhow::Result<Value> {
        self.handled.lock().push((name.to_string(), args, Some(std::thread::current().id())));
        if self.broken {
            return Ok(Value::Null);
        }
        Ok(json!({"ok": true, "status": "needs_input"}))
    }

    fn prepare_confirm(&self, conversation_id: &str, cart_hashes: &[String]) -> anyhow::Result<Prepared<Snap>> {
        let snapshot = Arc::new(SnapshotData { conversation: conversation_id.into(), hashes: cart_hashes.to_vec() });
        self.prepared.lock().push(snapshot.clone());
        self.prepare_entered.set();
        assert!(self.prepare_release.wait(Duration::from_secs(3)), "test did not release preparation");
        if let Some((result, with_snapshot)) = self.prepare_result.lock().clone() {
            return Ok(Prepared { result, snapshot: with_snapshot.then(|| snap("x", &[HASH])) });
        }
        Ok(Prepared {
            result: json!({"ok": true, "question": "Buy one item for USD 10.00?",
                           "about": "Purchase approval for one item, USD 10.00."}),
            snapshot: Some(snapshot),
        })
    }

    fn confirm(&self, snapshot: Snap, cancelled: &Arc<AtomicBool>) -> anyhow::Result<Value> {
        self.confirmed.lock().push((snapshot.clone(), cancelled.clone()));
        self.confirm_entered.set();
        assert!(self.dispatch_release.wait(Duration::from_secs(3)), "test did not release dispatch");
        if cancelled.load(Ordering::SeqCst) {
            return Ok(json!({"ok": false, "status": "not_submitted"}));
        }
        self.submitted.lock().push(snapshot);
        self.submit_entered.set();
        assert!(self.response_release.wait(Duration::from_secs(3)), "test did not release response");
        if let Some(error) = self.confirm_error.lock().clone() {
            return Err(anyhow::anyhow!(error));
        }
        if self.broken {
            return Ok(Value::Null);
        }
        Ok(json!({"ok": true, "status": "order_placed", "order_id": "order-1"}))
    }
}

/// An `asyncio.Event`.
#[derive(Clone)]
struct AsyncEvent(Arc<watch::Sender<bool>>);

impl AsyncEvent {
    fn new() -> Self {
        AsyncEvent(Arc::new(watch::channel(false).0))
    }

    fn set(&self) {
        self.0.send_replace(true);
    }

    fn is_set(&self) -> bool {
        *self.0.borrow()
    }

    async fn wait(&self) {
        let mut rx = self.0.subscribe();
        tokio::time::timeout(Duration::from_secs(3), rx.wait_for(|set| *set))
            .await
            .expect("event was not set")
            .expect("event dropped");
    }
}

/// The owner's question: records calls and waits for the test's answer.
struct Question {
    calls: Mutex<Vec<(String, String)>>,
    entered: AsyncEvent,
    closed: AsyncEvent,
    answer_tx: Mutex<Option<oneshot::Sender<String>>>,
    answer_rx: Mutex<Option<oneshot::Receiver<String>>>,
}

struct SetOnDrop(AsyncEvent);

impl Drop for SetOnDrop {
    fn drop(&mut self) {
        self.0.set();
    }
}

impl Question {
    fn new() -> Arc<Self> {
        let (tx, rx) = oneshot::channel();
        Arc::new(Question {
            calls: Mutex::new(Vec::new()),
            entered: AsyncEvent::new(),
            closed: AsyncEvent::new(),
            answer_tx: Mutex::new(Some(tx)),
            answer_rx: Mutex::new(Some(rx)),
        })
    }

    fn asker(self: &Arc<Self>) -> impl FnOnce(String, String) -> Pin<Box<dyn Future<Output = Result<String, AskError>> + Send>> + Send + 'static {
        let this = self.clone();
        move |question, about| {
            Box::pin(async move {
                this.calls.lock().push((question, about));
                this.entered.set();
                let _closed = SetOnDrop(this.closed.clone());
                let rx = this.answer_rx.lock().take().expect("question asked twice");
                Ok(rx.await.expect("answer dropped"))
            })
        }
    }

    fn answer(&self, text: &str) {
        let tx = self.answer_tx.lock().take().expect("answered twice");
        tx.send(text.to_string()).expect("question was withdrawn");
    }

    /// Python's `question.answer.cancelled()`: the waiting side was dropped.
    fn withdrawn(&self) -> bool {
        self.answer_tx.lock().as_ref().is_some_and(|tx| tx.is_closed())
    }

    fn answered(&self) -> bool {
        self.answer_tx.lock().is_none()
    }
}

fn yes() -> impl FnOnce(String, String) -> Pin<Box<dyn Future<Output = Result<String, AskError>> + Send>> + Send + 'static {
    |question: String, about: String| {
        Box::pin(async move {
            assert!(question.contains("USD 10.00") && about.contains("USD 10.00"));
            Ok("yes".to_string())
        })
    }
}

fn gateway(service: &Arc<TestService>) -> AgentCardTools<TestService> {
    AgentCardTools::new(service.clone())
}

async fn wait_thread(event: &Arc<TestService>, which: fn(&TestService) -> &Event) {
    let service = event.clone();
    let reached = tokio::task::spawn_blocking(move || which(&service).wait(Duration::from_secs(2))).await.unwrap();
    assert!(reached, "worker did not reach its test barrier");
}

/// Let spawned tasks run until `cond` holds (Python's `await asyncio.sleep(0)`).
async fn settle(cond: impl Fn() -> bool) {
    for _ in 0..2000 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("condition never held");
}

fn spawn_handle<F, Fut>(
    gateway: &AgentCardTools<TestService>,
    name: &str,
    args: Value,
    chat: Option<i64>,
    ask: F,
) -> tokio::task::JoinHandle<Value>
where
    F: FnOnce(String, String) -> Fut + Send + 'static,
    Fut: Future<Output = Result<String, AskError>> + Send + 'static,
{
    let gateway = gateway.clone();
    let name = name.to_string();
    tokio::spawn(async move { gateway.handle(&name, &args, chat, ask).await })
}

fn status(out: &Value) -> &str {
    out["status"].as_str().unwrap_or("")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_nonconfirm_dispatch_is_threaded_and_never_asks() {
    for name in ["agentcard_merchants", "agentcard_buy", "agentcard_status"] {
        let service = TestService::new();
        let gateway = gateway(&service);
        let question = Question::new();
        let caller = std::thread::current().id();
        let out = gateway.handle(name, &json!({"ask": "one item"}), Some(OWNER), question.asker()).await;
        assert_eq!(out, json!({"ok": true, "status": "needs_input"}));
        let handled = service.handled.lock();
        assert_eq!(handled.len(), 1);
        assert_eq!((handled[0].0.as_str(), &handled[0].1), (name, &json!({"ask": "one item"})));
        assert_ne!(handled[0].2, Some(caller));
        assert!(question.calls.lock().is_empty() && service.prepared() == 0 && service.confirmed() == 0);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_only_one_exact_prepared_snapshot_reaches_confirmation() {
    for answer in ["yes", "ok", "go ahead please", "yes, thank you"] {
        let service = TestService::new();
        let (gateway, question) = (gateway(&service), Question::new());
        let task = spawn_handle(&gateway, "agentcard_confirm", args(), Some(OWNER), question.asker());
        question.entered.wait().await;
        assert!(service.confirmed() == 0 && service.submitted() == 0);
        assert_eq!(
            *question.calls.lock(),
            vec![("Buy one item for USD 10.00?".to_string(), "Purchase approval for one item, USD 10.00.".to_string())]
        );
        question.answer(answer);
        let out = task.await.unwrap();
        assert_eq!(out, json!({"ok": true, "status": "order_placed", "order_id": "order-1"}), "{answer}");
        assert!(service.confirmed() == 1 && service.submitted() == 1);
        let confirmed = service.confirmed.lock();
        assert!(Arc::ptr_eq(&confirmed[0].0, &service.prepared.lock()[0]));
        assert!(!confirmed[0].1.load(Ordering::SeqCst) && !gateway.active(None));
        assert!(out.get("snapshot").is_none() && out.get("question").is_none());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_ambiguous_denied_or_timed_out_answers_cannot_confirm() {
    for answer in [
        "no",
        "",
        "maybe",
        "yeah no",
        "yes but cheaper",
        "ok wait",
        "ok also update the README",
        "sure go for it I guess",
        "no (no answer within 180 seconds)",
    ] {
        let (service, question) = (TestService::new(), Question::new());
        let gateway = gateway(&service);
        let task = spawn_handle(&gateway, "agentcard_confirm", args(), Some(OWNER), question.asker());
        question.entered.wait().await;
        question.answer(answer);
        assert_eq!(status(&task.await.unwrap()), "not_submitted", "{answer:?}");
        assert!(service.prepared() == 1 && service.confirmed() == 0 && service.submitted() == 0);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_failed_owner_question_does_not_confirm_or_expose_the_error() {
    for timeout in [true, false] {
        let ask = move |_question: String, _about: String| async move {
            if timeout {
                Err(AskError::Timeout)
            } else {
                Err(AskError::Failed(anyhow::anyhow!("private-provider-token")))
            }
        };
        let service = TestService::new();
        let out = gateway(&service).handle("agentcard_confirm", &args(), Some(OWNER), ask).await;
        assert!(out["ok"] == json!(false) && status(&out) == "not_submitted");
        assert!(!out.to_string().contains("private-provider-token"));
        assert_eq!(service.confirmed(), 0);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_unready_or_malformed_preparation_cannot_ask_or_confirm() {
    let cases = [
        (json!({"ok": false, "status": "needs_input"}), false),
        (json!({"ok": true}), false),
        (json!({"ok": true, "question": "", "about": "safe"}), true),
    ];
    for prepared in cases {
        let (service, question) = (TestService::new(), Question::new());
        *service.prepare_result.lock() = Some(prepared.clone());
        let out = gateway(&service).handle("agentcard_confirm", &args(), Some(OWNER), question.asker()).await;
        assert!(out["ok"] == json!(false) && question.calls.lock().is_empty() && service.confirmed() == 0, "{prepared:?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_model_arguments_never_supply_approval_or_an_invalid_selection() {
    let mut approved = args();
    approved["approved"] = json!(true);
    let cases = [
        json!([]),
        json!({}),
        json!({"conversation_id": "x", "cart_hashes": []}),
        json!({"conversation_id": "", "cart_hashes": [HASH]}),
        json!({"conversation_id": "x", "cart_hashes": [HASH.to_uppercase()]}),
        json!({"conversation_id": "x", "cart_hashes": [HASH, HASH]}),
        json!({"conversation_id": "x", "cart_hashes": vec![HASH; 9]}),
        approved,
        json!({"conversation_id": "x", "cart_hashes": [1]}),
    ];
    for case in cases {
        let (service, question) = (TestService::new(), Question::new());
        let out = gateway(&service).handle("agentcard_confirm", &case, Some(OWNER), question.asker()).await;
        assert_eq!(status(&out), "not_submitted", "{case}");
        assert!(service.prepared() == 0 && service.confirmed() == 0 && question.calls.lock().is_empty());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_argument_mutation_while_waiting_cannot_change_the_approved_snapshot() {
    let (service, question) = (TestService::new(), Question::new());
    let mut args = json!({"conversation_id": "original", "cart_hashes": [HASH]});
    let gateway = gateway(&service);
    let task = spawn_handle(&gateway, "agentcard_confirm", args.clone(), Some(OWNER), question.asker());
    question.entered.wait().await;
    args["conversation_id"] = json!("replacement");
    args["cart_hashes"][0] = json!(OTHER_HASH);
    question.answer("yes");
    assert_eq!(task.await.unwrap()["ok"], json!(true));
    let submitted = service.submitted.lock();
    assert_eq!(submitted.len(), 1);
    assert_eq!(*submitted[0], *snap("original", &[HASH]));
}

/// Rust borrows the caller's arguments only until the first await: the gateway
/// copies the selection before handing it to the worker.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn selection_is_copied_from_the_borrowed_arguments() {
    let service = TestService::new();
    let out = {
        let args = json!({"conversation_id": "original", "cart_hashes": [HASH]});
        gateway(&service).handle("agentcard_confirm", &args, Some(OWNER), yes()).await
    };
    assert_eq!(out["status"], json!("order_placed"));
    assert_eq!(*service.prepared.lock()[0], *snap("original", &[HASH]));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_confirmations_are_serialized_and_cross_chat_stop_only_invalidates_its_queue() {
    let (service, question) = (TestService::new(), Question::new());
    let gateway = gateway(&service);
    let first = spawn_handle(&gateway, "agentcard_confirm", args(), Some(OWNER), question.asker());
    question.entered.wait().await;
    let second = spawn_handle(&gateway, "agentcard_confirm", args(), Some(OTHER_OWNER), yes());
    settle(|| gateway.active(Some(OTHER_OWNER))).await;
    assert!(service.prepared() == 1 && gateway.active(Some(OTHER_OWNER)));
    assert_eq!(gateway.stop(Some(OTHER_OWNER)).stopped, 1);
    assert!(!question.answered() && service.confirmed() == 0);
    question.answer("yes");
    assert_eq!(first.await.unwrap()["ok"], json!(true));
    assert_eq!(status(&second.await.unwrap()), "not_submitted");
    assert!(service.prepared() == 1 && service.confirmed() == 1);
    assert!(!gateway.active(None));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_stop_withdraws_the_owner_prompt_and_every_queued_confirmation() {
    let (service, question) = (TestService::new(), Question::new());
    let gateway = gateway(&service);
    let first = spawn_handle(&gateway, "agentcard_confirm", args(), Some(OWNER), question.asker());
    question.entered.wait().await;
    let second = spawn_handle(&gateway, "agentcard_confirm", args(), Some(OWNER), yes());
    settle(|| gateway.inner.requests.lock().len() == 2).await;
    let stopped = gateway.stop(Some(OWNER));
    assert!(stopped.stopped == 2 && !stopped.in_flight);
    assert_eq!(stopped.line, "Stopped. No pending purchase confirmation will be submitted.");
    assert_eq!(status(&first.await.unwrap()), "not_submitted");
    assert_eq!(status(&second.await.unwrap()), "not_submitted");
    question.closed.wait().await;
    assert!(question.closed.is_set() && question.withdrawn());
    assert!(service.prepared() == 1 && service.confirmed() == 0);
    assert!(!gateway.active(None));
}

/// Rust cannot suppress an abort the way a coroutine can swallow
/// `CancelledError`; the late "yes" here arrives from a question that resolves
/// right after Stop, and the post-answer check still refuses it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_question_that_suppresses_cancellation_cannot_return_a_late_yes_to_purchase() {
    let service = TestService::new();
    let entered = AsyncEvent::new();
    let gateway = gateway(&service);
    let stopped_gate = AsyncEvent::new();
    let (entered2, gate2) = (entered.clone(), stopped_gate.clone());
    let stubborn_question = move |_question: String, _about: String| async move {
        entered2.set();
        gate2.wait().await;
        Ok("yes".to_string())
    };
    let task = spawn_handle(&gateway, "agentcard_confirm", args(), Some(OWNER), stubborn_question);
    entered.wait().await;
    assert_eq!(gateway.stop(Some(OWNER)).stopped, 1);
    stopped_gate.set();
    assert_eq!(status(&task.await.unwrap()), "not_submitted");
    assert!(service.confirmed() == 0 && !gateway.active(None));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_stop_during_preparation_prevents_the_late_prompt_and_submission() {
    let (service, question) = (TestService::new(), Question::new());
    service.prepare_release.clear();
    let gateway = gateway(&service);
    let task = spawn_handle(&gateway, "agentcard_confirm", args(), Some(OWNER), question.asker());
    wait_thread(&service, |s| &s.prepare_entered).await;
    assert_eq!(gateway.stop(Some(OWNER)).stopped, 1);
    service.prepare_release.set();
    assert_eq!(status(&task.await.unwrap()), "not_submitted");
    assert!(question.calls.lock().is_empty() && service.confirmed() == 0);
    service.release();
    gateway.wait_idle().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_stop_immediately_after_owner_answer_is_checked_before_confirm_dispatch() {
    let service = TestService::new();
    let gateway = gateway(&service);
    let inside = gateway.clone();
    let answer = move |_question: String, _about: String| async move {
        inside.stop(Some(OWNER));
        Ok("yes".to_string())
    };
    let out = gateway.handle("agentcard_confirm", &args(), Some(OWNER), answer).await;
    assert!(status(&out) == "not_submitted" && service.confirmed() == 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_stop_signals_a_confirm_worker_before_it_submits() {
    let service = TestService::new();
    service.dispatch_release.clear();
    let gateway = gateway(&service);
    let task = spawn_handle(&gateway, "agentcard_confirm", args(), Some(OWNER), yes());
    wait_thread(&service, |s| &s.confirm_entered).await;
    let out = gateway.stop(Some(OWNER));
    assert!(out.in_flight && out.stopped == 0);
    assert!(service.confirmed.lock()[0].1.load(Ordering::SeqCst));
    service.dispatch_release.set();
    assert_eq!(status(&task.await.unwrap()), "not_submitted");
    assert_eq!(service.submitted(), 0);
    service.release();
    gateway.wait_idle().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_stop_after_dispatch_reports_uncertainty_and_preserves_the_real_order_result() {
    let service = TestService::new();
    service.response_release.clear();
    let gateway = gateway(&service);
    let task = spawn_handle(&gateway, "agentcard_confirm", args(), Some(OWNER), yes());
    wait_thread(&service, |s| &s.submit_entered).await;
    let out = gateway.stop(Some(OWNER));
    assert!(out.stopped == 0 && out.in_flight);
    assert!(out.line.contains("cannot undo") && out.line.contains("check"));
    assert!(!out.line.contains("purchase was cancelled"));
    assert_eq!(
        out.line,
        "A purchase request may already be in flight. Stopping cannot undo it; check its AgentCard status before trying again."
    );
    assert!(gateway.active(Some(OWNER)) && !gateway.active(Some(OTHER_OWNER)));
    service.response_release.set();
    assert_eq!(status(&task.await.unwrap()), "order_placed");
    assert!(service.submitted() == 1 && service.confirmed() == 1);
    assert!(!gateway.active(None));
    service.release();
    gateway.wait_idle().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_caller_cancellation_withdraws_approval_and_unlocks_the_next_request() {
    let (service, question) = (TestService::new(), Question::new());
    let gateway = gateway(&service);
    let task = spawn_handle(&gateway, "agentcard_confirm", args(), Some(OWNER), question.asker());
    question.entered.wait().await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    gateway.wait_idle().await;
    assert!(question.closed.is_set() && service.confirmed() == 0);
    assert_eq!(gateway.handle("agentcard_confirm", &args(), Some(OWNER), yes()).await["ok"], json!(true));
    assert_eq!(service.confirmed(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_caller_cancellation_during_preparation_retains_serialization_until_worker_finishes() {
    let service = TestService::new();
    service.prepare_release.clear();
    let gateway = gateway(&service);
    let first = spawn_handle(&gateway, "agentcard_confirm", args(), Some(OWNER), yes());
    wait_thread(&service, |s| &s.prepare_entered).await;
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    let second = spawn_handle(&gateway, "agentcard_confirm", args(), Some(OWNER), yes());
    settle(|| gateway.inner.requests.lock().len() == 2).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(service.prepared(), 1);
    service.prepare_release.set();
    assert_eq!(second.await.unwrap()["ok"], json!(true));
    assert!(service.prepared() == 2 && service.submitted() == 1);
    service.release();
    gateway.wait_idle().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_cancelled_queued_request_never_prepares_after_the_first_confirmation() {
    let (service, question) = (TestService::new(), Question::new());
    let gateway = gateway(&service);
    let first = spawn_handle(&gateway, "agentcard_confirm", args(), Some(OWNER), question.asker());
    question.entered.wait().await;
    let queued = spawn_handle(&gateway, "agentcard_confirm", args(), Some(OWNER), yes());
    settle(|| gateway.inner.requests.lock().len() == 2).await;
    queued.abort();
    assert!(queued.await.unwrap_err().is_cancelled());
    question.answer("yes");
    assert_eq!(first.await.unwrap()["ok"], json!(true));
    gateway.wait_idle().await;
    assert!(service.prepared() == 1 && service.submitted() == 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_inflight_caller_cancellation_retains_worker_and_blocks_then_invalidates_queued_request() {
    let service = TestService::new();
    service.response_release.clear();
    let gateway = gateway(&service);
    let first = spawn_handle(&gateway, "agentcard_confirm", args(), Some(OWNER), yes());
    wait_thread(&service, |s| &s.submit_entered).await;
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    let queued = spawn_handle(&gateway, "agentcard_confirm", args(), Some(OTHER_OWNER), yes());
    settle(|| gateway.active(Some(OTHER_OWNER))).await;
    assert!(gateway.active(Some(OWNER)) && service.prepared() == 1);
    let stopped = gateway.stop(None);
    assert!(stopped.in_flight && stopped.stopped == 1);
    assert_eq!(
        stopped.line,
        "Stopped pending purchase confirmations. A purchase request may already be in flight. \
Stopping cannot undo it; check its AgentCard status before trying again."
    );
    service.response_release.set();
    assert_eq!(status(&queued.await.unwrap()), "not_submitted");
    gateway.wait_idle().await;
    assert!(service.confirmed() == 1 && service.submitted() == 1 && !gateway.active(None));
    service.release();
    gateway.wait_idle().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_shutdown_stops_pending_work_and_closes_admission_for_every_tool() {
    let (service, question) = (TestService::new(), Question::new());
    let gateway = gateway(&service);
    let first = spawn_handle(&gateway, "agentcard_confirm", args(), Some(OWNER), question.asker());
    question.entered.wait().await;
    assert_eq!(gateway.shutdown().stopped, 1);
    assert_eq!(status(&first.await.unwrap()), "not_submitted");
    for name in ["agentcard_buy", "agentcard_status", "agentcard_merchants", "agentcard_confirm"] {
        assert_eq!(status(&gateway.handle(name, &args(), Some(OWNER), yes()).await), "not_submitted");
    }
    assert!(service.handled.lock().is_empty() && service.confirmed() == 0 && !gateway.active(None));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_unexpected_confirm_exception_is_unknown_without_retry_or_private_error() {
    let service = TestService::new();
    *service.confirm_error.lock() = Some("secret-payment-field".into());
    let out = gateway(&service).handle("agentcard_confirm", &args(), Some(OWNER), yes()).await;
    assert!(status(&out) == "unknown" && out["ok"] == json!(false));
    assert!(out["reason"].as_str().unwrap().contains("status") && !out.to_string().contains("secret-payment-field"));
    assert!(service.confirmed() == 1 && service.submitted() == 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_an_unusable_client_return_does_not_become_a_purchase_success() {
    let service = TestService::build(true);
    let gateway = gateway(&service);
    let out = gateway.handle("agentcard_status", &json!({"conversation_id": "x"}), Some(OWNER), yes()).await;
    assert_eq!(out, json!({"ok": false, "reason": "AgentCard did not return a usable result."}));
    let out = gateway.handle("agentcard_confirm", &args(), Some(OWNER), yes()).await;
    assert!(out["ok"] == json!(false) && status(&out) == "unknown");
    assert!(service.handled.lock().len() == 1 && service.confirmed() == 1 && service.submitted() == 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_unknown_tools_and_non_chat_principals_cannot_reach_the_client() {
    let service = TestService::new();
    let gateway = gateway(&service);
    assert_eq!(status(&gateway.handle("some_other_tool", &args(), Some(OWNER), yes()).await), "not_submitted");
    // Python also rejected `True` and `"101"`; the Rust signature only admits an
    // integer chat or none.
    let out = gateway.handle("agentcard_confirm", &args(), None, yes()).await;
    assert_eq!(out, not_submitted("An owner chat is required for purchase approval."));
    assert!(service.prepared() == 0 && service.handled.lock().is_empty() && service.confirmed() == 0);
}

/// A panicking client call is Python's `Exception`: no text, phase-dependent result.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_panicking_client_is_an_exception_without_text() {
    struct Panics;
    impl Service for Panics {
        type Snapshot = ();
        fn handle(&self, _: &str, _: Value) -> anyhow::Result<Value> {
            panic!("private-body")
        }
        fn prepare_confirm(&self, _: &str, _: &[String]) -> anyhow::Result<Prepared<()>> {
            panic!("private-body")
        }
        fn confirm(&self, _: (), _: &Arc<AtomicBool>) -> anyhow::Result<Value> {
            panic!("private-body")
        }
    }
    let gateway = AgentCardTools::new(Arc::new(Panics));
    let out = gateway.handle("agentcard_buy", &json!({}), Some(OWNER), yes()).await;
    assert_eq!(out, json!({"ok": false, "reason": "AgentCard did not return a usable result."}));
    let out = gateway.handle("agentcard_confirm", &args(), Some(OWNER), yes()).await;
    assert_eq!(out, not_submitted("AgentCard could not obtain owner approval; no confirmation was submitted."));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_object_arguments_and_messages_are_verbatim() {
    let service = TestService::new();
    let gateway = gateway(&service);
    assert_eq!(
        gateway.handle("agentcard_buy", &json!("x"), Some(OWNER), yes()).await,
        json!({"ok": false, "status": "not_submitted", "reason": "AgentCard arguments must be an object."})
    );
    assert_eq!(
        gateway.handle("agentcard_confirm", &json!({"conversation_id": " \x1c ", "cart_hashes": [HASH]}), Some(OWNER), yes()).await["reason"],
        json!("Name the conversation and one to eight distinct exact cart hashes.")
    );
    assert_eq!(gateway.stop(None).line, "No purchase confirmation is waiting.");
    assert_eq!(
        gateway.stop(None).to_value(),
        json!({"stopped": 0, "in_flight": false, "line": "No purchase confirmation is waiting."})
    );
    let timed_out = gateway
        .handle("agentcard_confirm", &args(), Some(OWNER), |_q: String, _a: String| async { Err(AskError::Timeout) })
        .await;
    assert_eq!(timed_out, not_submitted("The owner did not approve in time; no purchase was submitted."));
    let denied =
        gateway.handle("agentcard_confirm", &args(), Some(OWNER), |_q: String, _a: String| async { Ok("no".to_string()) }).await;
    assert_eq!(denied, not_submitted("The owner did not approve; do not retry this purchase automatically."));
    gateway.shutdown();
    assert_eq!(
        gateway.handle("agentcard_status", &json!({}), Some(OWNER), yes()).await["reason"],
        json!("AgentCard is shutting down; no new request was submitted.")
    );
}

#[test]
fn cart_hash_is_exact() {
    assert!(CART_HASH.is_match(HASH));
    for bad in ["0123456789abcde", "0123456789abcdef0", "0123456789ABCDEF", "0123456789abcdef\n"] {
        assert!(!CART_HASH.is_match(bad), "{bad:?}");
    }
}

/// The real client behind the gateway: an unknown conversation is the client's own
/// failure dict, returned as is, with no owner question and no HTTP request.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_client_failure_is_returned_without_asking_or_sending() {
    let dir = tempfile::tempdir().unwrap();
    let calls = Arc::new(Mutex::new(0usize));
    let seen = calls.clone();
    let transport = move |_request: eve_agentcard::HttpRequest| -> anyhow::Result<eve_agentcard::HttpResponse> {
        *seen.lock() += 1;
        Err(anyhow::anyhow!("offline"))
    };
    let config = eve_agentcard::AgentCardConfig::new(true, "test-token-value");
    let client = eve_agentcard::AgentCard::new(config, Some(Arc::new(transport)), Some(dir.path().join("state"))).unwrap();
    let gateway = AgentCardTools::new(Arc::new(client));
    let question = Question::new();
    let out = gateway.handle("agentcard_confirm", &args(), Some(OWNER), question.asker()).await;
    assert_eq!(out["ok"], json!(false));
    assert!(out.get("status").is_none(), "the client's dict is returned unchanged: {out}");
    assert!(question.calls.lock().is_empty() && *calls.lock() == 0 && !gateway.active(None));
}
