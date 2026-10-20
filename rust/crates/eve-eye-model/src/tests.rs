use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;

use eve_jev::{OpenError, Request};

use super::*;

const ORIGINAL: [&str; 11] =
    ["calm", "happy", "curious", "affection", "surprised", "sad", "worried", "skeptical", "frustrated", "excited", "wink"];
const ADDED: [&str; 7] = ["laughing", "sleepy", "confused", "sheepish", "relieved", "proud", "flattered"];
// designed, failed its blind bar (owner, 2026-10-12: "ship 7, drop thinking")
const DROPPED: [&str; 1] = ["thinking"];

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn design() -> Value {
    serde_json::from_str(&std::fs::read_to_string(repo().join("docs/expressions-design.json")).unwrap()).unwrap()
}

fn shipped() -> Vec<Value> {
    design()["labels"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|l| !DROPPED.contains(&l["name"].as_str().unwrap()))
        .cloned()
        .collect()
}

// ---- bridge/tests/test_eye_model_labels.py ----------------------------------------------------------------

#[test]
fn test_labels_are_the_original_eleven_then_the_seven_in_wire_order() {
    let expected: Vec<&str> = ORIGINAL.iter().chain(ADDED.iter()).copied().collect();
    assert_eq!(LABELS.to_vec(), expected);
    let names: Vec<String> = shipped().iter().map(|l| l["name"].as_str().unwrap().to_string()).collect();
    assert_eq!(names, ADDED.to_vec());
    assert!(!LABELS.contains(&"startled"));
}

#[test]
fn test_the_dropped_label_is_gone_everywhere() {
    let design = design();
    let names: Vec<&str> = design["labels"].as_array().unwrap().iter().map(|d| d["name"].as_str().unwrap()).collect();
    for label in DROPPED {
        assert!(names.contains(&label)); // the design keeps it as history
        assert!(!LABELS.contains(&label) && criterion(label).is_none() && ttl_ms(label).is_none());
        assert!(EYE_CHOICE["eye"]["criteria"].get(label).is_none() && QUESTION.get(label).is_none());
    }
}

#[test]
fn test_new_criteria_are_the_designs_text_and_the_old_ones_are_untouched() {
    for label in shipped() {
        assert_eq!(criterion(label["name"].as_str().unwrap()), label["criterion"].as_str());
    }
    assert_eq!(criterion("wink"), Some("playful teasing, a cheeky aside or asking for a wink"));
    assert_eq!(criterion("calm"), Some("neutral facts, routine questions or instructions"));
    assert_eq!(criterion("affection"), Some("love, gratitude, friendship or reassurance"));
    assert_eq!(criterion("curious"), Some("wondering, exploring or wanting to learn"));
    assert!(!criterion("wink").unwrap().contains("joke")); // laughing alone owns humour
}

#[test]
fn test_every_question_carries_every_label() {
    let keys: Vec<&str> = EYE_CHOICE["eye"]["criteria"].as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, LABELS.to_vec());
    let keys: Vec<&str> = QUESTION.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, LABELS.to_vec());
}

#[test]
fn test_ttls_follow_the_design_and_clamp() {
    let design: BTreeMap<&str, i64> = [
        ("laughing", 3000),
        ("sleepy", 5000),
        ("confused", 3500),
        ("sheepish", 4000),
        ("relieved", 3500),
        ("proud", 4000),
        ("flattered", 4000),
    ]
    .into_iter()
    .collect();
    for label in shipped() {
        // the design's prose says "TTL 3.5 s."; Python's `:g` of ms / 1000
        let ms = design[label["name"].as_str().unwrap()];
        let seconds = if ms % 1000 == 0 { format!("{}", ms / 1000) } else { format!("{}", ms as f64 / 1000.0) };
        assert!(label["shape"].as_str().unwrap().contains(&format!("TTL {seconds} s")));
    }
    let ttl_labels: Vec<&str> = TTL_MS.iter().map(|(l, _)| *l).collect();
    assert_eq!(ttl_labels, LABELS.to_vec());
    assert!(ORIGINAL.iter().all(|l| ttl_ms(l) == Some(4000)));
    let added: BTreeMap<&str, i64> = ADDED.iter().map(|l| (*l, ttl_ms(l).unwrap())).collect();
    assert_eq!(added, design);
    assert_eq!((TTL_MIN_MS, TTL_MAX_MS), (500, 6000));
}

#[test]
fn test_blind_state_is_built_as_production_builds_it() {
    // tools/jev_eyes_eval.blind_state joins whitespace and maps owner/eve onto the live worker's path; the
    // live worker's own path is this one.
    let mut context = ConversationContext::new();
    context.state("user", "I finally shipped it", 0.0);
    assert_eq!(context.state("assistant", "Nice work!", 1.0), "User: I finally shipped it\nEve: Nice work!");
    assert_eq!(ConversationContext::new().state("user", "hi", 0.0), "User: hi");
}

// ---- bridge/tests/test_live_expressions.py (the eye model's part) ---------------------------------------

#[test]
fn test_context_has_speaker_roles_and_expires() {
    let mut context = ConversationContext::new();
    context.state("user", "My dog died.", 0.0);
    let state = context.state("assistant", "I'm sorry to hear that.", 1.0);
    assert!(state.ends_with("Eve: I'm sorry to hear that."));
    assert!(state.contains("User: My dog died."));
    assert!(!context.state("assistant", "What next?", 91.0).contains("My dog died"));
}

type Seen = Arc<Mutex<Vec<(Value, f64)>>>;

/// A jev.make_predict opener that answers the eye choice with the given probabilities, no socket.
fn jev_opener(probabilities: Value) -> (Arc<dyn Opener>, Seen) {
    let seen: Seen = Arc::default();
    let log = seen.clone();
    let opener = move |request: &Request, timeout: f64| -> Result<Vec<u8>, OpenError> {
        log.lock().push((serde_json::from_slice(&request.data).unwrap(), timeout));
        let probs = probabilities.as_object().cloned().unwrap_or_default();
        let top = probs
            .iter()
            .fold(None::<(&String, f64)>, |best, (k, v)| {
                let v = v.as_f64().unwrap_or(0.0);
                match best {
                    Some((_, b)) if b >= v => best,
                    _ => Some((k, v)),
                }
            })
            .map(|(k, _)| k.clone())
            .unwrap_or_default();
        let confidence = probs.get(&top).cloned().unwrap_or(json!(0.0));
        let body = json!({"answers": {"eye": {"choice": top, "probabilities": probabilities, "confidence": confidence}},
                          "usage": {"input_tokens": 500}});
        Ok(serde_json::to_vec(&body).unwrap())
    };
    (Arc::new(opener), seen)
}

/// A raw-reply opener: the body Jev sends back, verbatim.
fn raw_opener(body: Value) -> Arc<dyn Opener> {
    Arc::new(move |_: &Request, _: f64| -> Result<Vec<u8>, OpenError> { Ok(serde_json::to_vec(&body).unwrap()) })
}

fn openrouter_env() -> HashMap<String, String> {
    [("OPENROUTER_API_KEY", "k"), ("EVE_JEV_ROUTE", "openrouter")]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// Options that keep the meter and the spend ledger out of the process's own.
fn options(opener: Arc<dyn Opener>, dir: &Path) -> JevEyeOptions {
    JevEyeOptions {
        opener: Some(opener),
        meter: Some(Arc::new(Meter::new())),
        ledger: Some(eve_spend::Ledger::at(dir)),
        ..JevEyeOptions::default()
    }
}

/// `LiveExpressions.label`'s acceptance check: a distribution over LABELS.
fn accepted_by_the_worker(answer: &EyeAnswer) -> (String, f64) {
    let values = &answer.probabilities;
    assert_eq!(values.len(), LABELS.len());
    assert!(values.iter().all(|x| x.is_finite() && (0.0..=1.0).contains(x)));
    assert!((py_fsum(values) - 1.0).abs() <= 1e-4);
    let i = argmax(values);
    (LABELS[i].to_string(), values[i])
}

#[test]
fn test_jev_eye_model_asks_one_choice_over_the_eleven_labels_and_answers_in_label_order() {
    let dir = tempfile::tempdir().unwrap();
    // Jev may omit the zero labels
    let (opener, seen) = jev_opener(json!({"surprised": 0.9, "curious": 0.1}));
    let model = JevEyeModel::new(&openrouter_env(), options(opener, dir.path())).unwrap();
    let answer = model.predict("User: WHAT?! the server just went down??").unwrap();
    assert!(answer.label == "surprised" && answer.ms >= 0.0);
    assert_eq!(answer.probabilities.len(), LABELS.len());
    assert!((answer.probabilities.iter().sum::<f64>() - 1.0).abs() < 1e-9);
    assert!((answer.probabilities[label_index("surprised").unwrap()] - 0.9).abs() < 1e-12);
    let (label, top) = accepted_by_the_worker(&answer); // the worker accepts it as is
    assert!(label == "surprised" && (top - 0.9).abs() < 1e-12);
    let seen = seen.lock();
    assert_eq!(seen.len(), 1);
    let request = &seen[0].0;
    let keys: Vec<&String> = request["questions"].as_object().unwrap().keys().collect();
    assert_eq!(keys, ["eye"]);
    assert_eq!(request["questions"]["eye"]["type"], "choice");
    assert_eq!(request["questions"]["eye"]["criteria"], criteria_object());
    assert!(request["state"].as_str().unwrap().starts_with("User: WHAT"));
}

#[test]
fn test_jev_eye_model_refuses_an_answer_without_probabilities_and_needs_a_key() {
    let dir = tempfile::tempdir().unwrap();
    let (opener, _) = jev_opener(json!({}));
    let model = JevEyeModel::new(&openrouter_env(), options(opener.clone(), dir.path())).unwrap();
    let err = model.predict("User: hi").unwrap_err();
    assert_eq!(err.kind(), "ValueError");
    // no key: loud at construction, not per turn
    let env: HashMap<String, String> = [("EVE_JEV_ROUTE".to_string(), "openrouter".to_string())].into();
    let err = JevEyeModel::new(&env, options(opener, dir.path())).unwrap_err();
    assert_eq!(err.to_string(), "OPENROUTER_API_KEY is not set (route openrouter)");
}

// ---- parity ----------------------------------------------------------------------------------------------

/// Runs `code` in python3 with `eye_model.py` loaded as `m` (only its stdlib imports run), returns stdout.
fn python_eye_model(code: &str) -> String {
    let script = format!(
        "import importlib.util, json, sys\n\
         spec = importlib.util.spec_from_file_location('eye_model', sys.argv[1])\n\
         m = importlib.util.module_from_spec(spec)\n\
         spec.loader.exec_module(m)\n{code}\n"
    );
    let out = Command::new("python3")
        .arg("-I")
        .arg("-c")
        .arg(script)
        .arg(repo().join("bridge/src/eve_bridge/eye_model.py"))
        .output()
        .expect("python3 runs");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn constants_match_the_python_module_byte_for_byte() {
    let out = python_eye_model(
        "for v in (m.CRITERIA, list(m.LABELS), m.TTL_MS, m.QUESTION, m.EYE_CHOICE, m.WARM_STATE, \
         [m.DEFAULT_TTL_MS, m.TTL_MIN_MS, m.TTL_MAX_MS]):\n    print(json.dumps(v))",
    );
    let lines: Vec<&str> = out.lines().collect();
    let ttl = Value::Object(TTL_MS.iter().map(|(k, v)| (k.to_string(), json!(v))).collect());
    let ours = [
        eve_jev::py_dumps(&criteria_object()),
        eve_jev::py_dumps(&json!(LABELS)),
        eve_jev::py_dumps(&ttl),
        eve_jev::py_dumps(&QUESTION),
        eve_jev::py_dumps(&EYE_CHOICE),
        eve_jev::py_dumps(&json!(WARM_STATE)),
        eve_jev::py_dumps(&json!([DEFAULT_TTL_MS, TTL_MIN_MS, TTL_MAX_MS])),
    ];
    assert_eq!(lines.len(), ours.len());
    for (py, rs) in lines.iter().zip(ours.iter()) {
        assert_eq!(py, rs);
    }
}

#[test]
fn conversation_context_matches_python_on_a_long_script() {
    let mut script: Vec<(&str, String, f64)> = Vec::new();
    let roles = ["user", "assistant", "diary", "demo", "user", "user", "assistant"];
    let long = "é漢🙂".repeat(300) + "tail";
    for i in 0..60 {
        let who = roles[i % roles.len()];
        let text = if i % 9 == 0 { format!("{long}{i}") } else { format!("turn {i} from {who}") };
        script.push((who, text, (i as f64) * 17.5));
    }
    // ASCII JSON in a raw literal: json.loads joins the \ud83d\ude42 pairs (a plain literal would not).
    let payload = eve_jev::py_dumps(&json!(script.iter().map(|(w, t, n)| json!([w, t, n])).collect::<Vec<_>>()));
    let code = format!(
        "c = m.ConversationContext()\nfor who, text, now in json.loads(r'''{}'''):\n    print(json.dumps(c.state(who, text, now)))",
        payload
    );
    let out = python_eye_model(&code);
    let mut context = ConversationContext::new();
    let ours: Vec<String> =
        script.iter().map(|(w, t, n)| eve_jev::py_dumps(&json!(context.state(w, t, *n)))).collect();
    let theirs: Vec<&str> = out.lines().collect();
    assert_eq!(theirs.len(), ours.len());
    for (i, (py, rs)) in theirs.iter().zip(ours.iter()).enumerate() {
        assert_eq!(py, rs, "turn {i}");
    }
}

#[test]
fn context_keeps_the_first_seen_role_order_trims_to_400_and_skips_diary() {
    let mut context = ConversationContext::new();
    context.state("assistant", "first", 0.0);
    context.state("user", &"x".repeat(500), 1.0);
    context.state("assistant", "again", 2.0); // updated in place: the assistant stays first in the dict
    assert_eq!(context.turns.iter().map(|(r, _)| r.as_str()).collect::<Vec<_>>(), ["assistant", "user"]);
    let state = context.state("diary", "eve saw a cat", 91.0);
    assert_eq!(state, format!("Eve: again\nUser: {}\nEve's observation: eve saw a cat", "x".repeat(400)));
    assert_eq!(context.turns.len(), 2); // diary is never remembered
    // 90 s exactly is still in; a moment later it is gone
    assert_eq!(context.state("demo", "d", 91.0), format!("Eve: again\nUser: {}\nUser: d", "x".repeat(400)));
    assert_eq!(context.state("demo", "d", 91.5), "Eve: again\nUser: d");
}

fn predict_raw(body: Value) -> Result<EyeAnswer, EyeError> {
    let dir = tempfile::tempdir().unwrap();
    JevEyeModel::new(&openrouter_env(), options(raw_opener(body), dir.path())).unwrap().predict("User: hi")
}

#[test]
fn probabilities_are_read_with_pythons_float_and_falsiness() {
    // Strings are float()ed, falsy values are 0, True is 1; the result is normalised.
    let a = predict_raw(json!({"answers": {"eye": {"probabilities":
        {"happy": " 0.5\n", "sad": "1_0e-1", "calm": true, "wink": null, "curious": "", "worried": [], "excited": 0}}}}))
    .unwrap();
    let total = 0.5 + 1.0 + 1.0;
    assert_eq!(a.label, "calm");
    assert_eq!(a.probabilities[label_index("happy").unwrap()], 0.5 / total);
    assert_eq!(a.probabilities[label_index("sad").unwrap()], 1.0 / total);
    // A tie goes to the first label in LABELS order.
    let a = predict_raw(json!({"answers": {"eye": {"probabilities": {"wink": 0.5, "happy": 0.5}}}})).unwrap();
    assert_eq!(a.label, "happy");
    let errors = [
        (json!({"happy": "lots"}), "ValueError", "could not convert string to float: 'lots'"),
        (json!({"happy": "1__0"}), "ValueError", "could not convert string to float: '1__0'"),
        (json!({"happy": [1]}), "TypeError", "float() argument must be a string or a real number, not 'list'"),
        (json!({"happy": {"a": 1}}), "TypeError", "float() argument must be a string or a real number, not 'dict'"),
        (json!({"happy": 0.5, "sad": -0.1}), "ValueError", "jev returned unusable eye probabilities"),
        (json!({"happy": "nan"}), "ValueError", "jev returned unusable eye probabilities"),
        (json!({"happy": "-Infinity", "sad": 1}), "ValueError", "jev returned unusable eye probabilities"),
        (json!({"happy": 0, "sad": 0.0}), "ValueError", "jev returned unusable eye probabilities"),
        (json!({"thinking": 1}), "ValueError", "jev returned unusable eye probabilities"),
    ];
    for (probs, kind, message) in errors {
        let err = predict_raw(json!({"answers": {"eye": {"probabilities": probs}}})).unwrap_err();
        assert_eq!((err.kind(), err.to_string().as_str()), (kind, message), "{probs}");
    }
}

#[test]
fn a_missing_or_malformed_answer_raises_as_python_does() {
    for body in [
        json!({}),
        json!({"answers": null}),
        json!({"answers": []}),
        json!({"answers": {}}),
        json!({"answers": {"eye": "happy"}}),
        json!({"answers": {"eye": {"probabilities": [0.5, 0.5]}}}),
        json!({"answers": {"other": {"probabilities": {"happy": 1}}}}),
    ] {
        let err = predict_raw(body.clone()).unwrap_err();
        assert_eq!((err.kind(), err.to_string()), ("ValueError", "jev returned no eye probabilities".into()), "{body}");
    }
    // A truthy `answers` that is not a dict has no .get.
    let err = predict_raw(json!({"answers": [1]})).unwrap_err();
    assert_eq!((err.kind(), err.to_string()), ("AttributeError", "'list' object has no attribute 'get'".into()));
    let err = predict_raw(json!({"answers": "x"})).unwrap_err();
    assert_eq!(err.to_string(), "'str' object has no attribute 'get'");
}

#[test]
fn a_jev_failure_passes_through_by_its_class() {
    let dir = tempfile::tempdir().unwrap();
    let opener: Arc<dyn Opener> = Arc::new(|_: &Request, _: f64| -> Result<Vec<u8>, OpenError> { Ok(b"[]".to_vec()) });
    let model = JevEyeModel::new(&openrouter_env(), options(opener, dir.path())).unwrap();
    let err = model.predict("User: hi").unwrap_err();
    assert_eq!(err.kind(), "JevError");
    assert!(matches!(err, EyeError::Jev(PredictFailure::Jev(_))));
}

#[test]
fn the_timeout_is_the_argument_else_eve_jev_timeout_else_three_seconds() {
    let dir = tempfile::tempdir().unwrap();
    let timeout_of = |env: &HashMap<String, String>, timeout_s: f64| {
        let (opener, seen) = jev_opener(json!({"calm": 1.0}));
        let mut opts = options(opener, dir.path());
        opts.timeout_s = timeout_s;
        JevEyeModel::new(env, opts).unwrap().predict("User: hi").unwrap();
        let t = seen.lock()[0].1;
        t
    };
    let mut env = openrouter_env();
    assert_eq!(timeout_of(&env, 0.0), 3.0);
    env.insert("EVE_JEV_TIMEOUT".into(), "1.5".into());
    assert_eq!(timeout_of(&env, 0.0), 1.5);
    assert_eq!(timeout_of(&env, 0.25), 0.25);
}

#[test]
fn the_model_and_route_come_from_jev_route_config() {
    let dir = tempfile::tempdir().unwrap();
    let (opener, seen) = jev_opener(json!({"calm": 1.0}));
    let mut env = openrouter_env();
    env.insert("EVE_JEV_MODEL".into(), " pinned/model ".into());
    let model = JevEyeModel::new(&env, options(opener.clone(), dir.path())).unwrap();
    assert_eq!(model.model, "pinned/model");
    model.predict(WARM_STATE).unwrap();
    assert_eq!(seen.lock()[0].0["model"], "pinned/model");
    assert_eq!(seen.lock()[0].0["state"], WARM_STATE);
    env.insert("EVE_JEV_ROUTE".into(), "nope".into());
    let err = JevEyeModel::new(&env, options(opener, dir.path())).unwrap_err();
    assert_eq!(err.to_string(), "EVE_JEV_ROUTE must be one of ('typesafe', 'openrouter'), not 'nope'");
}

#[test]
fn python_float_strings() {
    for (text, want) in [
        ("1", Some(1.0)),
        (" +1.5 ", Some(1.5)),
        ("1e-3", Some(0.001)),
        (".5", Some(0.5)),
        ("5.", Some(5.0)),
        ("1_000", Some(1000.0)),
        ("inf", Some(f64::INFINITY)),
        ("-Infinity", Some(f64::NEG_INFINITY)),
        ("1_", None),
        ("_1", None),
        ("0x10", None),
        ("1e", None),
        ("", None),
        ("   ", None),
        ("in", None),
        ("1 2", None),
    ] {
        assert_eq!(py_float_str(text), want, "{text:?}");
    }
    assert!(py_float_str("-nan").unwrap().is_nan());
    assert!(py_float_str("NaN").unwrap().is_nan());
}

#[test]
fn sums_like_python_and_argmax_takes_the_first() {
    assert_eq!(py_fsum(&[0.1; 10]), 1.0); // Neumaier: a plain loop gives 0.9999999999999999
    assert_eq!(py_fsum(&[1e100, 1.0, -1e100]), 1.0);
    assert_eq!(argmax(&[0.2, 0.4, 0.4]), 1);
    assert_eq!(argmax(&[0.0]), 0);
}

#[test]
fn the_answer_serialises_as_pythons_dict() {
    let a = EyeAnswer { label: "calm".into(), probabilities: vec![1.0], ms: 2.5 };
    assert_eq!(a.to_value().to_string(), r#"{"label":"calm","probabilities":[1.0],"ms":2.5}"#);
}

// ---- local Laya, through the helper with fake laya_mlx and mlx modules ------------------------------------

const FAKE_LAYA: &str = r#"
import json, os
from . import agent as _agent_module  # noqa: F401

LOG = os.environ["FAKE_LAYA_LOG"]


def log(*entry):
    with open(LOG, "a") as f:
        f.write(json.dumps(entry) + "\n")


class _Scorer:
    def set_dtype(self, dtype):
        log("set_dtype", dtype)

    def parameters(self):
        return "params"


class _Model:
    scorer = _Scorer()


class _Tok:
    pad_token_id = 7


class Agent:
    def __init__(self, path, **kwargs):
        log("Agent", path, kwargs)
        if "missing" in path:
            raise FileNotFoundError("no weights at " + path)
        print("laya says hello on stdout")
        self.model, self.tok, self.cfg = _Model(), _Tok(), {"max_len": 50}

    def prepare(self, state, question):
        log("prepare", state, list(question), question["sleepy"])
        n = 60 if "too long" in state else 3
        return [{"ids": [0] * n, "label": k, "state": state} for k in question], None

    def forward(self, batch):
        items, pad = batch
        assert pad == 7
        state = items[0]["state"]
        if state == "die":
            os._exit(3)
        rows = []
        for item in items:
            yes = 0.0
            if item["label"] == "sleepy" and "goodnight" in state.lower():
                yes = 4.0
            if item["label"] == "calm":
                yes = 1.0
            if state == "nonfinite" and item["label"] == "sad":
                yes = float("inf")
            rows.append([0.5, yes + 0.5])
        return rows, None
"#;

const FAKE_AGENT: &str = r#"
def collate_items(items, pad):
    return items, pad
"#;

const FAKE_MLX_CORE: &str = r#"
import json, os

float32 = "float32"


def eval(x):
    with open(os.environ["FAKE_LAYA_LOG"], "a") as f:
        f.write(json.dumps(["eval", x]) + "\n")
"#;

struct Fake {
    dir: tempfile::TempDir,
}

impl Fake {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("lib");
        std::fs::create_dir_all(lib.join("laya_mlx")).unwrap();
        std::fs::create_dir_all(lib.join("mlx")).unwrap();
        std::fs::write(lib.join("laya_mlx/__init__.py"), FAKE_LAYA).unwrap();
        std::fs::write(lib.join("laya_mlx/agent.py"), FAKE_AGENT).unwrap();
        std::fs::write(lib.join("mlx/__init__.py"), "").unwrap();
        std::fs::write(lib.join("mlx/core.py"), FAKE_MLX_CORE).unwrap();
        Fake { dir }
    }

    fn env(&self) -> Vec<(OsString, OsString)> {
        vec![
            ("PYTHONPATH".into(), self.dir.path().join("lib").into_os_string()),
            ("FAKE_LAYA_LOG".into(), self.dir.path().join("log.jsonl").into_os_string()),
        ]
    }

    fn log(&self) -> Vec<Value> {
        std::fs::read_to_string(self.dir.path().join("log.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn load(&self, path: &str) -> Result<LiveEyeModel, EyeError> {
        // The system python3 has numpy (the helper's only real dependency here).
        LiveEyeModel::with_python(path, "python3", self.env())
    }
}

/// numpy's softmax over the fake's scores, computed here independently.
fn softmax(scores: &[f64]) -> Vec<f64> {
    let max = scores.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let e: Vec<f64> = scores.iter().map(|s| (s - max).exp()).collect();
    let sum: f64 = e.iter().sum();
    e.iter().map(|x| x / sum).collect()
}

#[test]
fn laya_loads_as_python_does_and_answers_in_label_order() {
    let fake = Fake::new();
    let model = fake.load("/models/laya").unwrap();
    let log = fake.log();
    assert_eq!(log[0], json!(["Agent", "/models/laya", {"dtype": "float16", "device": "gpu", "compile": false}]));
    assert_eq!(log[1], json!(["set_dtype", "float32"]));
    assert_eq!(log[2], json!(["eval", "params"]));

    let answer = model.predict("User: Goodnight, eve.").unwrap();
    assert_eq!(answer.label, "sleepy");
    let mut scores = vec![0.0; LABELS.len()];
    scores[label_index("sleepy").unwrap()] = 4.0;
    scores[label_index("calm").unwrap()] = 1.0;
    for (got, want) in answer.probabilities.iter().zip(softmax(&scores)) {
        assert!((got - want).abs() < 1e-15);
    }
    accepted_by_the_worker(&answer);
    let prepare = &fake.log()[3];
    assert_eq!(prepare[0], "prepare");
    assert_eq!(prepare[1], "User: Goodnight, eve.");
    assert_eq!(prepare[2], json!(LABELS));
    assert_eq!(prepare[3], QUESTION["sleepy"]);

    // Calm wins a turn with no other signal.
    assert_eq!(model.predict(WARM_STATE).unwrap().label, "calm");
}

#[test]
fn laya_refuses_an_over_budget_context_and_nonfinite_scores_then_keeps_answering() {
    let fake = Fake::new();
    let model = fake.load("/models/laya").unwrap();
    let err = model.predict("this is too long").unwrap_err();
    assert_eq!((err.kind(), err.to_string()), ("ValueError", "eye context exceeds token budget".into()));
    let err = model.predict("nonfinite").unwrap_err();
    assert_eq!((err.kind(), err.to_string()), ("ValueError", "nonfinite eye scores".into()));
    assert_eq!(model.predict("goodnight").unwrap().label, "sleepy");
}

#[test]
fn a_failed_laya_load_raises_its_own_exception() {
    let fake = Fake::new();
    let err = fake.load("/models/missing").unwrap_err();
    assert_eq!((err.kind(), err.to_string()), ("FileNotFoundError", "no weights at /models/missing".into()));
    // laya-mlx not installed at all: the import error, as Python's constructor raises it.
    let empty = tempfile::tempdir().unwrap();
    let err = LiveEyeModel::with_python(
        "/models/laya",
        "python3",
        vec![("PYTHONPATH".into(), empty.path().as_os_str().to_owned()), ("PYTHONNOUSERSITE".into(), "1".into())],
    )
    .unwrap_err();
    assert_eq!(err.kind(), "ModuleNotFoundError");
    assert_eq!(err.to_string(), "No module named 'laya_mlx'");
}

#[test]
fn a_missing_interpreter_is_a_load_failure() {
    let err = LiveEyeModel::with_python("/models/laya", "/nonexistent/python3", Vec::new()).unwrap_err();
    assert_eq!(err.kind(), "FileNotFoundError");
}

#[test]
fn a_helper_that_dies_is_reloaded_on_the_next_turn() {
    let fake = Fake::new();
    let model = fake.load("/models/laya").unwrap();
    let err = model.predict("die").unwrap_err();
    assert_eq!(err.kind(), "RuntimeError");
    assert!(err.to_string().starts_with("laya eye helper exited"), "{err}");
    assert_eq!(model.predict("goodnight").unwrap().label, "sleepy");
    let loads = fake.log().iter().filter(|e| e[0] == "Agent").count();
    assert_eq!(loads, 2);
}

#[test]
#[ignore = "live: needs laya-mlx (EVE_EXPRESSION_PYTHON or the bridge venv) and ~/.config/eve-bridge/models/laya-multilingual-mlx"]
fn the_real_laya_checkpoint_matches_the_python_model() {
    let home = std::env::var("HOME").unwrap();
    let path = format!("{home}/.config/eve-bridge/models/laya-multilingual-mlx");
    let model = LiveEyeModel::new(&path).unwrap();
    let state = "User: My dog died.\nEve: I'm so sorry.";
    let answer = model.predict(state).unwrap();
    let code = format!(
        "sys.path.insert(0, {src})\nfrom eve_bridge.eye_model import LiveEyeModel\n\
         print(json.dumps(LiveEyeModel({path}).predict({state})))",
        src = eve_jev::py_dumps(&json!(repo().join("bridge/src").to_string_lossy())),
        path = eve_jev::py_dumps(&json!(path)),
        state = eve_jev::py_dumps(&json!(state)),
    );
    let out = Command::new(default_python())
        .arg("-c")
        .arg(format!("import json, sys\n{code}"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let line = String::from_utf8(out.stdout).unwrap();
    let python: Value = serde_json::from_str(line.lines().last().unwrap()).unwrap();
    assert_eq!(python["label"], answer.label);
    assert_eq!(python["probabilities"], json!(answer.probabilities));
}
