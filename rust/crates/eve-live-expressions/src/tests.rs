use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;

use eve_jev::{Meter, OpenError, Opener, Request};

use super::*;

// ---- helpers (bridge/tests/test_live_expressions.py) ---------------------------------------------------------

fn happy() -> Vec<f64> {
    let mut v = vec![0.0; LABELS.len()];
    v[1] = 1.0;
    v
}

fn one_hot(label: &str) -> Vec<f64> {
    let mut v = vec![0.0; LABELS.len()];
    v[eve_eye_model::label_index(label).unwrap()] = 1.0;
    v
}

/// `class Model`: always happy, 1 ms; records the states it was asked about.
#[derive(Default)]
struct Model {
    states: Mutex<Vec<String>>,
}

impl EyeModel for Model {
    fn predict(&self, state: &str) -> Result<EyeAnswer, EyeError> {
        self.states.lock().push(state.to_string());
        Ok(EyeAnswer { label: String::new(), probabilities: happy(), ms: 1.0 })
    }
}

/// A model answering a fixed distribution.
struct Fixed(Vec<f64>);

impl EyeModel for Fixed {
    fn predict(&self, _: &str) -> Result<EyeAnswer, EyeError> {
        Ok(EyeAnswer { label: String::new(), probabilities: self.0.clone(), ms: 1.0 })
    }
}

fn factory_of(model: Arc<dyn EyeModel>) -> Option<ModelFactory> {
    Some(Arc::new(move || Ok(LoadedModel::new(model.clone()))))
}

fn model_factory() -> Option<ModelFactory> {
    factory_of(Arc::new(Model::default()))
}

type Sent = Arc<Mutex<Vec<Value>>>;

/// `async def send(cmd): sent.append(cmd); return result`.
fn recorder(result: bool) -> (SendFn, Sent) {
    let sent: Sent = Arc::default();
    let log = sent.clone();
    let send = send_fn(move |cmd| {
        let log = log.clone();
        async move {
            log.lock().push(cmd);
            Ok(result)
        }
    });
    (send, sent)
}

fn env_of(pairs: &[(&str, &str)]) -> Arc<dyn Env + Send + Sync> {
    let map: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    Arc::new(map)
}

/// An empty env: no backend, no switch file override, no HOME surprises.
fn quiet_env() -> Option<Arc<dyn Env + Send + Sync>> {
    Some(env_of(&[("HOME", "/home/test")]))
}

fn manual_clock(start: f64) -> (Clock, Arc<Mutex<f64>>) {
    let now = Arc::new(Mutex::new(start));
    let read = now.clone();
    (Arc::new(move || *read.lock()), now)
}

async fn until(predicate: impl Fn() -> bool) {
    for _ in 0..500 {
        if predicate() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("worker did not reach expected state");
}

async fn stop(task: tokio::task::JoinHandle<()>) {
    task.abort();
    let _ = task.await;
}

fn spawn(service: &Arc<LiveExpressions>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(service.clone().run())
}

// ---- bridge/tests/test_live_expressions.py ---------------------------------------------------------------------

async fn queue_and_arbitration(muted: bool, phase: &'static str, chirp: bool) {
    let dir = tempfile::tempdir().unwrap();
    let (send, sent) = recorder(true);
    let service = Arc::new(LiveExpressions::new(
        send,
        Options {
            path: Some(dir.path().join("settings.json")),
            model_factory: model_factory(),
            muted: Arc::new(move || muted),
            phase: Arc::new(move || phase.to_string()),
            env: quiet_env(),
            ..Options::default()
        },
    ));
    service.set_enabled(true).await.unwrap();
    service.offer("assistant", "An earlier obsolete sentence.");
    let event = service.offer("assistant", "This is such wonderful news!");
    let task = spawn(&service);
    until(|| sent.lock().len() == 1).await;
    {
        let sent = sent.lock();
        assert!(sent[0]["id"] == json!(event.unwrap()) && sent[0]["label"] == "happy");
        assert_eq!(sent[0]["chirp"], json!(chirp));
        let ttl = sent[0]["ttl_ms"].as_i64().unwrap();
        assert!((500..=4000).contains(&ttl));
    }
    assert_eq!(service.dropped(), 1);
    assert_eq!(service.offer("assistant", "This is such wonderful news!"), None);
    service.set_enabled(false).await.unwrap();
    assert_eq!(sent.lock().last().unwrap()["clear"], json!(true));
    assert_eq!(service.offer("user", "hello"), None);
    assert_eq!(service.status()["phase"], json!(phase));
    assert_eq!(service.muted(), muted);
    stop(task).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_queue_and_arbitration() {
    // parametrized: (muted, phase, chirp)
    queue_and_arbitration(false, "speaking", false).await;
    queue_and_arbitration(true, "speaking", false).await;
    queue_and_arbitration(false, "listening", false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_disabled_during_inference_drops_result() {
    struct Slow {
        started: Mutex<mpsc::Sender<()>>,
        release: Mutex<mpsc::Receiver<()>>,
    }
    impl EyeModel for Slow {
        fn predict(&self, state: &str) -> Result<EyeAnswer, EyeError> {
            let _ = self.started.lock().send(());
            let _ = self.release.lock().recv_timeout(Duration::from_secs(2));
            Model::default().predict(state)
        }
    }
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let slow = Arc::new(Slow { started: Mutex::new(started_tx), release: Mutex::new(release_rx) });
    let started = Arc::new(Mutex::new(started_rx));

    let dir = tempfile::tempdir().unwrap();
    let sent: Sent = Arc::default();
    let log = sent.clone();
    // `async def send(cmd): sent.append(cmd)` returns None: not a failure.
    let send = send_fn(move |cmd| {
        let log = log.clone();
        async move {
            log.lock().push(cmd);
            Ok(true)
        }
    });
    let service = Arc::new(LiveExpressions::new(
        send,
        Options {
            path: Some(dir.path().join("settings.json")),
            model_factory: factory_of(slow),
            env: quiet_env(),
            ..Options::default()
        },
    ));
    service.set_enabled(true).await.unwrap();
    service.offer("user", "Wonderful news!");
    let task = spawn(&service);
    let s = started.clone();
    until(move || s.lock().try_recv().is_ok()).await;
    service.set_enabled(false).await.unwrap();
    release_tx.send(()).unwrap();
    until(|| service.dropped() == 1).await;
    {
        let sent = sent.lock();
        assert!(sent.len() == 1 && sent[0]["clear"] == json!(true));
    }
    stop(task).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_stale_and_send_failure() {
    let dir = tempfile::tempdir().unwrap();
    let (clock, now) = manual_clock(1.0);
    let (send, _) = recorder(false);
    let service = Arc::new(LiveExpressions::new(
        send,
        Options {
            path: Some(dir.path().join("settings.json")),
            model_factory: model_factory(),
            clock,
            env: quiet_env(),
            ..Options::default()
        },
    ));
    service.set_enabled(true).await.unwrap();
    service.offer("diary", "Something interesting here.");
    *now.lock() = 10.0;
    let task = spawn(&service);
    until(|| service.dropped() == 1).await;
    service.offer("user", "A new interesting thing!");
    until(|| service.dropped() == 2).await;
    assert!(service.sent() == 0 && service.last().is_none() && service.error().contains("send failed"));
    assert_eq!(service.error(), "RuntimeError: expression send failed");
    stop(task).await;
}

#[test]
fn test_invalid_results() {
    let mut nan = vec![0.0; 6];
    nan[0] = f64::NAN;
    let cases = [json!([]), json!([1, 1, 1, 1, 1, 1]), json!([null, 0, 0, 0, 0, 0]), json!([true, 0, 0, 0, 0, 0])];
    for values in cases {
        let err = LiveExpressions::label(&json!({"probabilities": values})).unwrap_err();
        assert_eq!(err.kind, "ValueError");
    }
    // NaN cannot ride in JSON; the typed path refuses it too.
    assert!(LiveExpressions::label_values(&nan).is_err());
    let mut full_nan = vec![0.0; LABELS.len()];
    full_nan[0] = f64::NAN;
    full_nan[1] = 1.0;
    assert!(LiveExpressions::label_values(&full_nan).is_err());
    let mut with_bool: Vec<Value> = happy().into_iter().map(Value::from).collect();
    with_bool[0] = json!(false);
    assert!(LiveExpressions::label(&json!({"probabilities": with_bool})).is_err());
}

#[test]
fn test_every_expression_is_accepted_without_startle() {
    assert!(!LABELS.contains(&"startled"));
    for (i, label) in LABELS.iter().enumerate() {
        let mut values = vec![0; LABELS.len()];
        values[i] = 1;
        assert_eq!(LiveExpressions::label(&json!({"probabilities": values})).unwrap(), (*label, 1.0));
    }
}

#[test]
fn test_context_has_speaker_roles_and_expires() {
    let mut context = ConversationContext::new();
    context.state("user", "My dog died.", 0.0);
    let state = context.state("assistant", "I'm sorry to hear that.", 1.0);
    assert!(state.ends_with("Eve: I'm sorry to hear that."));
    assert!(state.contains("User: My dog died."));
    assert!(!context.state("assistant", "What next?", 91.0).contains("My dog died"));
}

/// A jev.make_predict opener that answers the eye choice with the given probabilities, no socket.
fn jev_opener(probabilities: Value) -> Arc<dyn Opener> {
    Arc::new(move |_: &Request, _: f64| -> Result<Vec<u8>, OpenError> {
        let body = json!({"answers": {"eye": {"choice": "surprised", "probabilities": probabilities,
                                              "confidence": 0.9}}, "usage": {"input_tokens": 500}});
        Ok(serde_json::to_vec(&body).unwrap())
    })
}

#[test]
fn test_jev_eye_model_answer_is_accepted_by_the_worker_as_is() {
    let dir = tempfile::tempdir().unwrap();
    let env: HashMap<String, String> =
        [("OPENROUTER_API_KEY", "k"), ("EVE_JEV_ROUTE", "openrouter")].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let options = JevEyeOptions {
        opener: Some(jev_opener(json!({"surprised": 0.9, "curious": 0.1}))),
        meter: Some(Arc::new(Meter::new())),
        ledger: Some(eve_spend::Ledger::at(dir.path())),
        ..JevEyeOptions::default()
    };
    let model = JevEyeModel::new(&env, options).unwrap();
    let answer = model.predict("User: WHAT?! the server just went down??").unwrap();
    let (label, top) = LiveExpressions::label_values(&answer.probabilities).unwrap();
    assert!(label == "surprised" && (top - 0.9).abs() < 1e-12);
    let (label, _) = LiveExpressions::label(&answer.to_value()).unwrap();
    assert_eq!(label, "surprised");
}

#[test]
fn test_the_expression_backend_is_jev_unless_laya_is_spelled_out() {
    let dir = tempfile::tempdir().unwrap();
    for (value, expect) in [("", "jev"), ("jev", "jev"), ("laya", "laya"), (" Laya ", "laya"), ("mlx", "jev")] {
        let env: HashMap<String, String> = [("EVE_EXPRESSION_BACKEND".to_string(), value.to_string())].into();
        assert_eq!(backend_from_env(&env), expect);
        let service = LiveExpressions::new(
            recorder(true).0,
            Options {
                path: Some(dir.path().join("settings.json")),
                model_factory: model_factory(),
                env: Some(env_of(&[("EVE_EXPRESSION_BACKEND", value), ("HOME", "/home/test")])),
                ..Options::default()
            },
        );
        assert!(service.backend == expect && service.status()["backend"] == json!(expect));
    }
}

// ---- bridge/tests/test_eye_model_labels.py (this crate's part) --------------------------------------------------

#[test]
fn test_ttls_follow_the_design_and_clamp() {
    assert_eq!(expression_ttl_ms("sleepy", 0.0), 5000);
    assert_eq!(expression_ttl_ms("laughing", 0.5), 2500);
    assert_eq!(expression_ttl_ms("happy", 3.9), 500); // never below the floor
    assert_eq!(expression_ttl_ms("happy", 0.0), 4000);
    assert_eq!(expression_ttl_ms("not-a-label", 0.0), 500);
    assert_eq!(expression_ttl_ms("happy", 0.0004), 3999); // int() truncates
    assert_eq!(expression_ttl_ms("sleepy", -10.0), 6000); // never above the ceiling
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_the_picker_sends_the_labels_own_ttl() {
    let (send, sent) = recorder(true);
    let service = Arc::new(LiveExpressions::new(
        send,
        Options {
            model_factory: factory_of(Arc::new(Fixed(one_hot("sleepy")))),
            enabled: Some(true),
            lazy: true,
            clock: Arc::new(|| 100.0),
            env: quiet_env(),
            ..Options::default()
        },
    ));
    service.offer("user", "Goodnight, eve.");
    let task = spawn(&service);
    until(|| !sent.lock().is_empty()).await;
    stop(task).await;
    let sent = sent.lock();
    assert!(sent[0]["label"] == "sleepy" && sent[0]["ttl_ms"] == json!(5000));
}

#[test]
fn test_every_label_is_a_valid_pick() {
    for (i, label) in LABELS.iter().enumerate() {
        let mut values = vec![0.0; LABELS.len()];
        values[i] = 1.0;
        assert_eq!(LiveExpressions::label(&json!({"probabilities": values})).unwrap(), (*label, 1.0));
    }
}

// ---- bridge/tests/test_phone_expressions.py (this crate's part) -------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_switching_off_clears_the_face_and_writes_no_file() {
    let dir = tempfile::tempdir().unwrap();
    let board_file = dir.path().join("expressions.json");
    let (send, sent) = recorder(true);
    let service = Arc::new(LiveExpressions::new(
        send,
        Options {
            enabled: Some(true),
            lazy: true,
            model_factory: model_factory(),
            env: Some(env_of(&[("EVE_EXPRESSIONS_FILE", board_file.to_str().unwrap())])),
            ..Options::default()
        },
    ));
    assert!(service.path.is_none());
    let task = spawn(&service);
    service.offer("assistant", "Great news, it shipped!");
    until(|| service.sent() == 1).await;
    service.set_enabled(false).await.unwrap();
    let cleared = sent.lock().last().cloned().unwrap();
    assert_eq!(cleared, json!({"cmd": "expression", "id": cleared["id"], "clear": true}));
    assert_eq!(service.offer("user", "anything"), None);
    assert!(!board_file.exists()); // the board's switch file is not the phone's
    stop(task).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_no_live_call_sends_nothing() {
    let (send, sent) = recorder(true);
    let service = Arc::new(LiveExpressions::new(
        send,
        Options {
            connected: Arc::new(|| false),
            enabled: Some(true),
            lazy: true,
            model_factory: model_factory(),
            env: quiet_env(),
            ..Options::default()
        },
    ));
    let task = spawn(&service);
    assert!(service.offer("assistant", "Great news, it shipped!").is_some());
    until(|| service.dropped() == 1).await;
    assert!(sent.lock().is_empty() && service.sent() == 0);
    stop(task).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_model_fault_sends_nothing() {
    struct Broken;
    impl EyeModel for Broken {
        fn predict(&self, _: &str) -> Result<EyeAnswer, EyeError> {
            Err(EyeError::Raised { kind: "TimeoutError".into(), message: "jev timed out".into() })
        }
    }
    let (send, sent) = recorder(true);
    let service = Arc::new(LiveExpressions::new(
        send,
        Options { enabled: Some(true), lazy: true, model_factory: factory_of(Arc::new(Broken)), env: quiet_env(), ..Options::default() },
    ));
    let task = spawn(&service);
    service.offer("user", "what's on my calendar");
    until(|| service.dropped() >= 1 && service.error().contains("jev timed out")).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(sent.lock().is_empty() && service.sent() == 0);
    assert_eq!(service.error(), "TimeoutError: jev timed out");
    stop(task).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_late_pick_sends_nothing() {
    // The model answered after the 4 s window: stale, dropped, never sent.
    struct Late(Arc<Mutex<f64>>);
    impl EyeModel for Late {
        fn predict(&self, state: &str) -> Result<EyeAnswer, EyeError> {
            *self.0.lock() += 5.0;
            Model::default().predict(state)
        }
    }
    let (clock, now) = manual_clock(100.0);
    let (send, sent) = recorder(true);
    let service = Arc::new(LiveExpressions::new(
        send,
        Options { enabled: Some(true), lazy: true, model_factory: model_factory(), clock, env: quiet_env(), ..Options::default() },
    ));
    {
        let mut st = service.state();
        st.model = Some(LoadedModel::new(Arc::new(Late(now))));
        st.ready = true;
    }
    let task = spawn(&service);
    service.offer("assistant", "That is wonderful.");
    until(|| service.dropped() == 1).await;
    assert!(sent.lock().is_empty());
    stop(task).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_lazy_picker_loads_only_at_the_first_offer_and_a_failed_load_drops_it() {
    let loads = Arc::new(AtomicUsize::new(0));
    let counter = loads.clone();
    let factory: ModelFactory = Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
        Err(Raised::new("RuntimeError", "no Jev key"))
    });
    let (send, sent) = recorder(true);
    let service = Arc::new(LiveExpressions::new(
        send,
        Options { enabled: Some(true), lazy: true, model_factory: Some(factory), env: quiet_env(), ..Options::default() },
    ));
    let task = spawn(&service);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(loads.load(Ordering::SeqCst), 0); // no call from the phone, no model call
    service.offer("user", "Hello there.");
    until(|| loads.load(Ordering::SeqCst) == 1 && service.dropped() == 1).await;
    assert!(service.pending().is_none() && sent.lock().is_empty() && service.error().contains("no Jev key"));
    assert!(!service.ready());
    stop(task).await;
}

#[test]
fn test_the_phone_switch() {
    for (value, on) in
        [(None, true), (Some(""), true), (Some("1"), true), (Some("0"), false), (Some("off"), false), (Some("False"), false), (Some("no"), false)]
    {
        let env: HashMap<String, String> = match value {
            None => HashMap::new(),
            Some(v) => [("EVE_PHONE_EXPRESSIONS".to_string(), v.to_string())].into(),
        };
        assert_eq!(phone_expressions_on(&env), on, "{value:?}");
    }
    let env: HashMap<String, String> = [("EVE_PHONE_EXPRESSIONS".to_string(), " OFF\n".to_string())].into();
    assert!(!phone_expressions_on(&env));
}

#[test]
fn test_react_offer_returns_the_pending_event() {
    // test_react_goes_to_the_phone_when_no_board_is_connected: the id answered is the pending turn's.
    let dir = tempfile::tempdir().unwrap();
    let board = LiveExpressions::new(
        recorder(true).0,
        Options { path: Some(dir.path().join("board.json")), model_factory: model_factory(), env: quiet_env(), ..Options::default() },
    );
    let live = Arc::new(Mutex::new(true));
    let flag = live.clone();
    let phone = LiveExpressions::new(
        recorder(true).0,
        Options {
            connected: Arc::new(move || *flag.lock()),
            enabled: Some(true),
            lazy: true,
            model_factory: model_factory(),
            env: quiet_env(),
            ..Options::default()
        },
    );
    assert!(!board.enabled() && board.offer("demo", "That is wonderful news!").is_none()); // off
    let id = phone.offer("demo", "That is wonderful news!").unwrap();
    assert_eq!(phone.pending().unwrap().event, id);
    assert!(board.pending().is_none());
    assert!(phone.connected() && phone.status()["enabled"] == json!(true));
    *live.lock() = false;
    assert!(!phone.connected());
}

// ---- parity -----------------------------------------------------------------------------------------------------

#[test]
fn the_switch_file_is_read_as_python_reads_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("expressions.json");
    let make = || {
        LiveExpressions::new(
            recorder(true).0,
            Options { path: Some(path.clone()), model_factory: model_factory(), env: quiet_env(), ..Options::default() },
        )
    };
    assert!(!make().enabled()); // missing
    for (text, on) in [
        ("{\"enabled\": true}\n", true),
        ("{\"enabled\": 1}", false),
        ("{\"enabled\": \"true\"}", false),
        ("[true]", false),
        ("true", false),
        ("not json", false),
        ("{\"enabled\": false}", false),
    ] {
        std::fs::write(&path, text).unwrap();
        assert_eq!(make().enabled(), on, "{text}");
    }
    std::fs::write(&path, [0xff, 0xfe]).unwrap(); // not UTF-8: UnicodeDecodeError is a ValueError
    assert!(!make().enabled());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_enabled_writes_the_switch_atomically_in_pythons_format() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested/dir/expressions.json");
    let service = LiveExpressions::new(
        recorder(true).0,
        Options { path: Some(path.clone()), model_factory: model_factory(), env: quiet_env(), ..Options::default() },
    );
    service.set_enabled(true).await.unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"enabled\": true}\n");
    assert!(!path.with_extension("tmp").exists());
    service.set_enabled(false).await.unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"enabled\": false}\n");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_switch_that_cannot_be_written_raises_an_os_error() {
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("file");
    std::fs::write(&blocker, "x").unwrap();
    let service = LiveExpressions::new(
        recorder(true).0,
        Options { path: Some(blocker.join("expressions.json")), model_factory: model_factory(), env: quiet_env(), ..Options::default() },
    );
    let err = service.set_enabled(true).await.unwrap_err();
    assert!(err.kind == "FileExistsError" || err.kind == "NotADirectoryError", "{err:?}");
    assert!(err.message.starts_with("[Errno "), "{}", err.message);
    assert!(service.enabled()); // the state changed before the write, as Python's does
}

#[test]
fn the_switch_path_comes_from_the_env_and_is_expanded() {
    let service = LiveExpressions::new(
        recorder(true).0,
        Options { model_factory: model_factory(), env: Some(env_of(&[("HOME", "/home/test/")])), ..Options::default() },
    );
    assert_eq!(service.path.as_deref(), Some(Path::new("/home/test/.config/eve-bridge/expressions.json")));
    assert_eq!(service.model_path, Path::new("/home/test/.config/eve-bridge/models/laya-multilingual-mlx"));
    let service = LiveExpressions::new(
        recorder(true).0,
        Options {
            model_factory: model_factory(),
            env: Some(env_of(&[
                ("HOME", "/h"),
                ("EVE_EXPRESSIONS_FILE", "~/x//y/./s.json/"),
                ("EVE_EXPRESSION_MODEL", ""),
                ("EVE_EXPRESSION_BACKEND", "laya"),
            ])),
            ..Options::default()
        },
    );
    assert_eq!(service.path.as_deref(), Some(Path::new("/h/x/y/s.json")));
    assert_eq!(service.model_path, Path::new("."));
    assert_eq!(service.status()["model"], json!("."));
}

#[test]
fn python_path_normalization() {
    let env: HashMap<String, String> = [("HOME".to_string(), "/Users/me".to_string())].into();
    for (raw, want) in [
        ("", "."),
        (".", "."),
        ("./a", "a"),
        ("a/", "a"),
        ("//a", "//a"),
        ("///a", "/a"),
        ("a/../b", "a/../b"),
        ("~", "/Users/me"),
        ("~/a/", "/Users/me/a"),
        ("~user/a", "~user/a"),
        ("/~/a", "/~/a"),
    ] {
        assert_eq!(py_path(raw, &env), PathBuf::from(want), "{raw}");
    }
}

#[test]
fn offer_normalizes_whitespace_keeps_the_last_800_and_skips_repeats() {
    let service = LiveExpressions::new(
        recorder(true).0,
        Options { enabled: Some(true), model_factory: model_factory(), env: quiet_env(), ..Options::default() },
    );
    assert_eq!(service.offer("robot", "hello"), None);
    assert_eq!(service.offer("user", " \t\u{1f}\u{3000}\n"), None);
    let first = service.offer("user", "  hello\u{1c}there\n\tfriend ").unwrap();
    assert!(service.pending().unwrap().state.ends_with("User: hello there friend"));
    assert_eq!(service.offer("user", "hello there friend"), None); // the same line, normalized
    assert_eq!(service.dropped(), 0);
    let long: String = "é".repeat(900);
    let second = service.offer("assistant", &long).unwrap();
    assert_eq!(second, first.wrapping_add(1).max(1));
    assert_eq!(service.dropped(), 1);
    let pending = service.pending().unwrap();
    assert_eq!(pending.who, "assistant");
    assert!(pending.state.ends_with(&format!("Eve: {}", "é".repeat(800))));
    assert!(pending.state.starts_with("User: hello there friend\n"));
    let diary = service.offer("diary", "the room went dark").unwrap();
    assert!(service.pending().unwrap().state.ends_with("Eve's observation: the room went dark"));
    assert_eq!(service.pending().unwrap().generation, 3);
    assert!(diary > 0);
}

#[test]
fn event_ids_are_32_bit_and_never_zero() {
    let service = LiveExpressions::new(
        recorder(true).0,
        Options { enabled: Some(true), model_factory: model_factory(), env: quiet_env(), ..Options::default() },
    );
    assert!(service.state().seq >= 1);
    service.state().seq = u32::MAX;
    assert_eq!(service.offer("user", "a"), Some(1));
    assert_eq!(service.offer("user", "b"), Some(2));
}

#[test]
fn observe_keeps_integer_ids_and_the_last_24() {
    let service = LiveExpressions::new(
        recorder(true).0,
        Options { enabled: Some(true), model_factory: model_factory(), env: quiet_env(), ..Options::default() },
    );
    service.observe(&json!({"id": "7", "label": "sad"}));
    service.observe(&json!({"id": 1.5}));
    service.observe(&json!({"label": "sad"}));
    assert!(service.status()["board"].is_null());
    service.observe(&json!({"id": true})); // isinstance(True, int)
    assert_eq!(service.status()["board"], json!({"id": true}));
    for i in 0..30 {
        service.observe(&json!({"id": i, "label": "happy"}));
    }
    let status = service.status();
    let history = status["board_history"].as_array().unwrap();
    assert_eq!(history.len(), 24);
    assert_eq!(history[0]["id"], json!(6));
    assert_eq!(status["board"], json!({"id": 29, "label": "happy"}));
}

#[test]
fn status_has_pythons_keys_in_order() {
    let service = LiveExpressions::new(
        recorder(true).0,
        Options { enabled: Some(false), model_factory: model_factory(), env: quiet_env(), ..Options::default() },
    );
    let status = service.status();
    let keys: Vec<&String> = status.as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        ["enabled", "ready", "backend", "model", "policy", "error", "sent", "dropped", "last", "board", "board_history", "phase"]
    );
    assert_eq!(status["policy"], json!("experimental top choice; not calibrated"));
    assert_eq!(status["model"], json!("")); // jev, no model yet
    assert_eq!(status["phase"], json!("idle"));
    service.state().model = Some(LoadedModel::named(Arc::new(Model::default()), "jev-1"));
    assert_eq!(service.status()["model"], json!("jev-1"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sent_cue_is_recorded_and_the_next_waits_its_gap() {
    let model = Arc::new(Model::default());
    let (send, sent) = recorder(true);
    let service = Arc::new(LiveExpressions::new(
        send,
        Options { enabled: Some(true), model_factory: factory_of(model.clone()), env: quiet_env(), ..Options::default() },
    ));
    let task = spawn(&service);
    let first = service.offer("user", "My dog died.").unwrap();
    until(|| sent.lock().len() == 1).await;
    let last = service.last().unwrap();
    let keys: Vec<&String> = last.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["id", "source", "label", "p_top", "model_ms", "event_to_send_ms", "chirp_requested"]);
    assert!(last["id"] == json!(first) && last["source"] == "user" && last["label"] == "happy");
    assert!(last["p_top"] == json!(1.0) && last["model_ms"] == json!(1.0) && last["chirp_requested"] == json!(false));
    let cmd = sent.lock()[0].clone();
    let keys: Vec<&String> = cmd.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["cmd", "id", "label", "ttl_ms", "chirp"]);
    let t0 = Instant::now();
    service.offer("assistant", "I'm sorry to hear that.");
    until(|| sent.lock().len() == 2).await;
    assert!(t0.elapsed() >= Duration::from_millis(900), "{:?}", t0.elapsed()); // the 1.2 s gap
    assert!(model.states.lock()[1].contains("User: My dog died.\nEve: I'm sorry to hear that."));
    assert_eq!(service.sent(), 2);
    assert!(service.status()["ready"] == json!(true));
    stop(task).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_invalid_answer_is_a_value_error() {
    let (send, sent) = recorder(true);
    let service = Arc::new(LiveExpressions::new(
        send,
        Options {
            enabled: Some(true),
            model_factory: factory_of(Arc::new(Fixed(vec![0.5; LABELS.len()]))),
            env: quiet_env(),
            ..Options::default()
        },
    ));
    let task = spawn(&service);
    service.offer("user", "hi");
    until(|| service.dropped() == 1).await;
    assert_eq!(service.error(), "ValueError: invalid expression probabilities");
    assert!(sent.lock().is_empty());
    stop(task).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_send_exception_is_reported_by_its_class() {
    let send = send_fn(|_| async { Err(Raised::new("ConnectionResetError", "peer gone")) });
    let service = Arc::new(LiveExpressions::new(
        send,
        Options { enabled: Some(true), model_factory: model_factory(), env: quiet_env(), ..Options::default() },
    ));
    let task = spawn(&service);
    service.offer("user", "hi");
    until(|| service.dropped() == 1).await;
    assert_eq!(service.error(), "ConnectionResetError: peer gone");
    stop(task).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_aborted_worker_is_closed_and_takes_no_offers() {
    let service = Arc::new(LiveExpressions::new(
        recorder(true).0,
        Options { enabled: Some(true), model_factory: model_factory(), env: quiet_env(), ..Options::default() },
    ));
    let task = spawn(&service);
    until(|| service.ready()).await; // not lazy: loads at start
    stop(task).await;
    assert!(service.state().closed);
    assert_eq!(service.offer("user", "hello"), None);
}

#[test]
fn errors_are_cut_to_180_characters() {
    let e = Raised::new("RuntimeError", "x".repeat(400));
    assert_eq!(e.describe().chars().count(), 180);
    assert!(e.describe().starts_with("RuntimeError: xxx"));
}

#[test]
fn python_reprs() {
    assert_eq!(py_repr("mlx"), "'mlx'");
    assert_eq!(py_repr("it's"), "\"it's\"");
    assert_eq!(py_repr("a'\"b"), "'a\\'\"b'");
    assert_eq!(py_repr("t\ta\\\u{1}"), "'t\\ta\\\\\\x01'");
    assert_eq!(py_float_repr(1.0), "1.0");
    assert_eq!(py_float_repr(203.5), "203.5");
    assert_eq!(py_float_repr(1e16), "1e+16");
    assert_eq!(py_float_repr(1.5e-5), "1.5e-05");
    assert_eq!(py_float_repr(0.0001), "0.0001");
}

#[test]
fn sums_like_python() {
    // sum([0.1] * 10) is 1.0 in Python 3.12+ (compensated), 0.9999999999999999 naively.
    assert_eq!(py_sum(&[0.1; 10]), 1.0);
    assert_eq!(py_sum(&[]), 0.0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_clock_answering_nan_drops_the_turn_as_int_raises() {
    let (send, sent) = recorder(true);
    let service = Arc::new(LiveExpressions::new(
        send,
        Options {
            enabled: Some(true),
            model_factory: model_factory(),
            clock: Arc::new(|| f64::NAN),
            env: quiet_env(),
            ..Options::default()
        },
    ));
    let task = spawn(&service);
    service.offer("user", "hi");
    until(|| service.dropped() == 1).await;
    assert_eq!(service.error(), "ValueError: cannot convert float NaN to integer");
    assert!(sent.lock().is_empty());
    stop(task).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_endless_gap_sleeps_instead_of_panicking() {
    // last_sent_at in the far future: delay is inf; asyncio.sleep(inf) just waits.
    let (clock, now) = manual_clock(0.0);
    let (send, sent) = recorder(true);
    let service = Arc::new(LiveExpressions::new(
        send,
        Options { enabled: Some(true), model_factory: model_factory(), clock, env: quiet_env(), ..Options::default() },
    ));
    service.state().last_sent_at = f64::INFINITY;
    *now.lock() = f64::NEG_INFINITY;
    let task = spawn(&service);
    service.offer("user", "hi");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!task.is_finished());
    assert!(sent.lock().is_empty() && service.pending().is_some());
    stop(task).await;
}
