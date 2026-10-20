//! Port of bridge/tests/test_intent.py, plus the intent half of test_spend_wiring.py and parity checks.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

/// What a fake was called with.
#[derive(Default)]
struct Seen(std::sync::Mutex<Vec<String>>);

impl Seen {
    fn push(&self, s: &str) {
        self.0.lock().unwrap().push(s.to_string());
    }
    fn get(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

const LEAVES: &[&str] = &[
    "Goodbye.", "bye", "Bye eve!", "okay bye bye", "Alright, thanks, bye.", "see you later",
    "good night eve", "that's all for now", "That's it, thanks.", "We're done here.",
    "stop listening", "Please stop listening now.", "go away", "just go", "go to sleep",
    "I want you to leave.", "I want you to leave now", "we need you to go away", "leave me alone",
    "you can go now", "You can leave.", "get lost", "end the conversation", "I'm done talking",
    "Later.", "talk to you later", "Okay, see ya.",
    "I'm heading home, bye.", "Got to run, see you later!", // a goodbye clause after the sentence, then punctuation
];
const MUTES: &[&str] = &[
    "mute", "Mute yourself.", "be quiet", "shh", "hush eve", "no more beeping",
    "turn your sound off", "stop making noise", "go silent", "sound off please",
];
const UNMUTES: &[&str] = &["unmute", "Unmute yourself.", "sound back on", "turn your sound back on", "you can make sounds again"];
// Same words, about something else: the table must not claim these.
const NOT_MINE: &[&str] = &[
    "leave the tab open", "leave it on the desk", "mute Spotify", "mute the video", "tell Sam goodbye",
    "say bye to mom on Messages", "put the Mac to sleep", "make the computer go to sleep",
    "open it later", "is that all?", "that's all the files I need", "turn the sound off on YouTube",
    "go to the settings page", "what do you see", "play some music",
    "can you go and check my email", "stop the music",
    "look up the weather", "turn the volume down", "turn it down", "turn off the lights",
    "look at this email", "turn on the TV", "look for the file on the desktop",
    // Questions and computer jobs that share words with a lesson request.
    "what is ownership in Rust", "what's 17 times 23", "teach the kids to say hi on Messages",
    "I learned a lot today", "I want to learn more about this email", "help me understand this file",
    "walk me through the settings page",
];
const LESSONS: &[&str] = &[
    "teach me about Rust ownership", "Hey eve, teach me about Rust ownership.", "quiz me on French verbs",
    "can you tutor me", "I want a lesson", "I want to learn fractions", "help me work through this proof",
    "walk me through recursion", "let's do a lesson", "Could you help me understand the chain rule?",
    "test me on the periodic table", "I'd like to practice long division", "start a lesson",
];
const LOOKS: &[&str] = &[
    "look to your left", "Look a bit up and to your left.", "look behind you", "turn around",
    "Look around, eve.", "look at me", "can you look up at the ceiling", "turn your head to the right",
    "look down at the desk", "now look over there", "face the window", "look straight ahead",
];

fn all_are(texts: &[&str], want: Option<&str>) {
    for t in texts {
        assert_eq!(fast_intent(t), want, "{t:?}");
    }
}

#[test]
fn test_head_moves() {
    all_are(LOOKS, Some(LOOK));
}

#[test]
fn test_lesson_requests() {
    all_are(LESSONS, Some(LESSON));
}

#[test]
fn test_leave_phrases() {
    all_are(LEAVES, Some(LEAVE));
}

#[test]
fn test_mute_phrases() {
    all_are(MUTES, Some(MUTE));
}

#[test]
fn test_unmute_phrases() {
    all_are(UNMUTES, Some(UNMUTE));
}

#[test]
fn test_requests_about_the_computer_are_not_intents() {
    all_are(NOT_MINE, None);
}

#[test]
fn test_empty_is_none() {
    assert_eq!(fast_intent(""), None);
    assert_eq!(fast_intent("  ...  "), None);
}

#[test]
fn test_parse_intent_bounds_and_unknown_labels() {
    assert_eq!(parse_intent(r#"{"intent":"leave","confidence":0.93}"#).unwrap(), ("leave", 0.93));
    assert_eq!(parse_intent(r#"{"intent":"dance","confidence":2}"#).unwrap(), ("none", 1.0));
    assert_eq!(parse_intent(r#"{"intent":"mute","confidence":"x"}"#).unwrap(), ("mute", 0.0));
}

struct FakeClient {
    answer: (String, f64),
    delay: Duration,
    raises: Option<String>,
    seen: Seen,
}

impl FakeClient {
    fn new(label: &str, conf: f64) -> Self {
        Self { answer: (label.to_string(), conf), delay: Duration::ZERO, raises: None, seen: Seen::default() }
    }
}

impl IntentClient for FakeClient {
    fn classify(&self, text: &str) -> anyhow::Result<(String, f64)> {
        self.seen.push(text);
        if !self.delay.is_zero() {
            std::thread::sleep(self.delay);
        }
        if let Some(msg) = &self.raises {
            anyhow::bail!("{msg}");
        }
        Ok(self.answer.clone())
    }
}

fn classifier(c: Arc<FakeClient>) -> Classifier {
    make_classifier(c, timeout(), MIN_CONFIDENCE)
}

#[tokio::test]
async fn test_classifier_returns_label_above_the_floor() {
    let c = Arc::new(FakeClient::new("leave", 0.8));
    assert_eq!(classifier(c.clone())("alright, I'm heading out".into()).await.as_deref(), Some(LEAVE));
    assert_eq!(c.seen.get(), vec!["alright, I'm heading out".to_string()]);
}

#[tokio::test]
async fn test_classifier_below_floor_or_none_is_no_intent() {
    assert_eq!(classifier(Arc::new(FakeClient::new("leave", 0.4)))("maybe later".into()).await, None);
    assert_eq!(classifier(Arc::new(FakeClient::new(NONE, 0.99)))("open Safari".into()).await, None);
}

#[tokio::test]
async fn test_classifier_timeout_and_error_are_no_intent() {
    let slow = Arc::new(FakeClient { delay: Duration::from_millis(300), ..FakeClient::new("leave", 0.9) });
    assert_eq!(make_classifier(slow, Duration::from_millis(50), MIN_CONFIDENCE)("hmm".into()).await, None);
    let broken = Arc::new(FakeClient { raises: Some("503".into()), ..FakeClient::new("leave", 0.9) });
    assert_eq!(classifier(broken)("hmm".into()).await, None);
}

#[tokio::test]
async fn test_long_turns_are_not_classified() {
    let c = Arc::new(FakeClient::new("leave", 0.9));
    let long = vec!["word"; 40].join(" ");
    assert_eq!(classifier(c.clone())(long).await, None);
    assert!(c.seen.get().is_empty());
}

/// Answers every create with a fixed reply and records the bodies.
struct FakeCreate {
    reply: Value,
    calls: Seen,
    count: AtomicUsize,
}

impl FakeCreate {
    fn new(reply: Value) -> Self {
        Self { reply, calls: Seen::default(), count: AtomicUsize::new(0) }
    }
}

impl Responses for Arc<FakeCreate> {
    fn create(&self, body: Value) -> anyhow::Result<Value> {
        self.calls.push(&body.to_string());
        self.count.fetch_add(1, Ordering::SeqCst);
        Ok(self.reply.clone())
    }
}

fn reply(text: &str, usage: Value) -> Value {
    json!({"output": [{"type": "message", "content": [{"type": "output_text", "text": text}]}], "usage": usage})
}

#[test]
fn test_request_is_not_stored_and_is_strict_json() {
    let body = OpenAIIntentClient::new("gpt-5.4-nano", Some("sk-test"), timeout()).unwrap().request("bye");
    assert_eq!(body["store"], json!(false));
    let fmt = &body["text"]["format"];
    assert_eq!(fmt["strict"], json!(true));
    let mut got: Vec<&str> =
        fmt["schema"]["properties"]["intent"]["enum"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    let mut want = LABELS.to_vec();
    got.sort();
    want.sort();
    assert_eq!(got, want);
}

#[test]
fn test_classify_parses_the_sdk_answer() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(FakeCreate::new(reply(r#"{"intent":"unmute","confidence":0.7}"#, json!({}))));
    let c = OpenAIIntentClient::with_client("gpt-5.4-nano", Box::new(fake.clone()))
        .with_ledger(eve_spend::Ledger::at(dir.path()));
    assert_eq!(c.classify("you can beep again").unwrap(), (UNMUTE, 0.7));
    assert_eq!(fake.count.load(Ordering::SeqCst), 1);
}

// ---- test_spend_wiring.py: test_the_voice_intent_classifier_is_metered ------------------------------------------

#[test]
fn test_the_voice_intent_classifier_is_metered() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = eve_spend::Ledger::at(dir.path());
    let fake = Arc::new(FakeCreate::new(
        reply(r#"{"intent": "none", "confidence": 0.1}"#, json!({"input_tokens": 1_000_000, "output_tokens": 0})),
    ));
    let c = OpenAIIntentClient::with_client("gpt-5.4-nano", Box::new(fake)).with_ledger(ledger.clone());
    c.classify("bye").unwrap();
    let today = chrono_free_today(&ledger);
    assert_eq!(today.len(), 1, "{today:?}");
    let row = &today[0];
    assert_eq!(row["f"], json!("voice"));
    assert!((row["usd"].as_f64().unwrap() - 0.20).abs() < 1e-9, "{row:?}");
    assert_eq!(row["p"], json!("openrouter"));
    assert_eq!(row["m"], json!("gpt-5.4-nano"));
}

/// Today's rows, whatever the date: the ledger holds one file per day and the test wrote only one.
fn chrono_free_today(ledger: &eve_spend::Ledger) -> Vec<serde_json::Map<String, Value>> {
    let mut rows = Vec::new();
    for entry in std::fs::read_dir(ledger.dir()).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        if let Some(day) = name.strip_suffix(".jsonl") {
            rows.extend(ledger.day_rows(day));
        }
    }
    rows
}

// ---- the request and the prompt are Python's, byte for byte -----------------------------------------------------

#[test]
fn the_prompt_is_byte_identical_to_python() {
    use sha2::{Digest, Sha256};
    assert_eq!(PROMPT.chars().count(), 1882);
    assert_eq!(
        hex::encode(Sha256::digest(PROMPT.as_bytes())),
        "94518ac092a38e0dbee24d97470e60131c18e4164dadd1251996d6bad08923f8"
    );
}

#[test]
fn the_schema_and_request_serialize_as_python_writes_them() {
    assert_eq!(
        serde_json::to_string(&*SCHEMA).unwrap(),
        r#"{"type":"object","properties":{"intent":{"type":"string","enum":["leave","mute","unmute","look","lesson","remember","notes","none"]},"confidence":{"type":"number"}},"required":["intent","confidence"],"additionalProperties":false}"#
    );
    let c = OpenAIIntentClient::with_client("m", Box::new(Arc::new(FakeCreate::new(json!({})))));
    let body = c.request("hi");
    let keys: Vec<&str> = body.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, ["model", "instructions", "input", "text", "max_output_tokens", "store"]);
    assert_eq!(body["input"], json!([{"role": "user", "content": [{"type": "input_text", "text": "hi"}]}]));
    assert_eq!(body["max_output_tokens"], json!(40));
    assert_eq!(body["text"]["format"]["name"], json!("intent"));
    assert_eq!(body["instructions"], json!(PROMPT));
}

// ---- parity details -----------------------------------------------------------------------------------------------

#[test]
fn normalize_matches_python() {
    // Python: normalize("Okay — Thank you, EVE’s   done!") == "okay thanks eve's done"
    assert_eq!(normalize("Okay \u{2014} Thank you, EVE\u{2019}s   done!"), "okay thanks eve's done");
    assert_eq!(normalize("  ...  "), "");
    assert_eq!(normalize("thank youuu"), "thank youuu");
    assert_eq!(normalize("\u{2018}quoted\u{2019}"), "'quoted'");
}

#[test]
fn remember_and_notes_phrases() {
    for t in ["remember that", "Please remember this.", "don't forget that", "keep that in mind", "note that down",
              "remember that forever", "okay, remember that"] {
        assert_eq!(fast_intent(t), Some(REMEMBER), "{t:?}");
    }
    for t in ["I remember that", "remember when you said that"] {
        assert_eq!(fast_intent(t), None, "{t:?}");
    }
    for t in ["start taking notes", "take notes", "can you take some notes", "note-taking time", "start recording"] {
        assert_eq!(fast_intent(t), Some(NOTES), "{t:?}");
    }
    assert_eq!(fast_intent("my notes are in the drawer"), None);
}

#[test]
fn parse_intent_follows_python_float_and_truthiness() {
    assert_eq!(parse_intent(r#"{"intent":"look","confidence":"0.75"}"#).unwrap(), ("look", 0.75));
    assert_eq!(parse_intent(r#"{"intent":"look","confidence":" 1_0 "}"#).unwrap(), ("look", 1.0));
    assert_eq!(parse_intent(r#"{"intent":"look","confidence":"nan"}"#).unwrap(), ("look", 1.0));
    assert_eq!(parse_intent(r#"{"intent":"look","confidence":"-inf"}"#).unwrap(), ("look", 0.0));
    assert_eq!(parse_intent(r#"{"intent":"look","confidence":"_1"}"#).unwrap(), ("look", 0.0));
    assert_eq!(parse_intent(r#"{"intent":"look","confidence":true}"#).unwrap(), ("look", 1.0));
    assert_eq!(parse_intent(r#"{"intent":"look","confidence":null}"#).unwrap(), ("look", 0.0));
    assert_eq!(parse_intent(r#"{"intent":"look","confidence":[1]}"#).unwrap(), ("look", 0.0));
    assert_eq!(parse_intent(r#"{"intent":"look","confidence":-3}"#).unwrap(), ("look", 0.0));
    assert_eq!(parse_intent(r#"{"intent":"look"}"#).unwrap(), ("look", 0.0));
    assert_eq!(parse_intent(r#"{"intent":"","confidence":0.9}"#).unwrap(), ("none", 0.9));
    assert_eq!(parse_intent(r#"{"intent":5,"confidence":0.9}"#).unwrap(), ("none", 0.9));
    assert_eq!(parse_intent("{}").unwrap(), ("none", 0.0));
    let e = parse_intent("[1]").unwrap_err();
    assert_eq!((e.py_name(), e.to_string().as_str()), ("AttributeError", "'list' object has no attribute 'get'"));
    assert_eq!(parse_intent("not json").unwrap_err().py_name(), "JSONDecodeError");
}

#[test]
fn an_empty_answer_is_none_and_output_text_joins_message_parts() {
    let r = json!({"output": [
        {"type": "reasoning", "content": [{"type": "output_text", "text": "x"}]},
        {"type": "message", "content": [{"type": "output_text", "text": "{\"intent\":"}, {"type": "refusal"}]},
        {"type": "message", "content": [{"type": "output_text", "text": "\"leave\",\"confidence\":1}"}]},
    ]});
    assert_eq!(output_text(&r), r#"{"intent":"leave","confidence":1}"#);
    let dir = tempfile::tempdir().unwrap();
    let c = OpenAIIntentClient::with_client("m", Box::new(Arc::new(FakeCreate::new(json!({"output": []})))))
        .with_ledger(eve_spend::Ledger::at(dir.path()));
    assert_eq!(c.classify("x").unwrap(), (NONE, 0.0));
}

#[test]
fn py_repr_quotes_like_python() {
    assert_eq!(py_repr("bye"), "'bye'");
    assert_eq!(py_repr("I'm out"), "\"I'm out\"");
    assert_eq!(py_repr("a'b\"c"), "'a\\'b\"c'");
    assert_eq!(py_repr("x\ny\u{a0}\u{7f}"), "'x\\ny\\xa0\\x7f'");
    assert_eq!(py_repr("caf\u{e9} \u{2014}"), "'caf\u{e9} \u{2014}'");
}

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

#[test]
fn make_intent_classifier_needs_the_key_and_respects_the_switch() {
    assert!(make_intent_classifier(Some(&env(&[]))).is_none());
    assert!(make_intent_classifier(Some(&env(&[("OPENROUTER_API_KEY", "  ")]))).is_none());
    for off in ["0", "false", " NO ", "off"] {
        assert!(make_intent_classifier(Some(&env(&[("OPENROUTER_API_KEY", "k"), ("EVE_INTENT", off)]))).is_none());
    }
    assert!(make_intent_classifier(Some(&env(&[("OPENROUTER_API_KEY", "k")]))).is_some());
    assert!(make_intent_classifier(Some(&env(&[("OPENROUTER_API_KEY", "k"), ("EVE_INTENT", "")]))).is_some());
    assert_eq!(model_from(Some(&env(&[("EVE_INTENT_MODEL", "  ")]))), DEFAULT_MODEL);
    assert_eq!(model_from(Some(&env(&[("EVE_INTENT_MODEL", " gpt-6-luna ")]))), "gpt-6-luna");
}

#[test]
fn the_real_client_is_on_openrouter_with_four_seconds_and_no_retries() {
    let c = eve_openrouter::async_client(
        Some("k"),
        eve_openrouter::ClientOptions::default().timeout(timeout()).max_retries(0),
    )
    .unwrap();
    assert_eq!(c.base_url(), eve_openrouter::BASE_URL);
    assert_eq!(c.timeout(), Duration::from_secs(4));
    assert_eq!(c.max_retries(), 0);
}

struct Scripted(Vec<anyhow::Result<(String, f64)>>, AtomicUsize);

impl IntentClient for Scripted {
    fn classify(&self, _text: &str) -> anyhow::Result<(String, f64)> {
        let i = self.1.fetch_add(1, Ordering::SeqCst);
        match &self.0[i] {
            Ok(a) => Ok(a.clone()),
            Err(e) => Err(anyhow::anyhow!("{e}")),
        }
    }
}

fn pairs(p: &[(&str, &str)]) -> Vec<(String, String)> {
    p.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
}

#[test]
fn run_intent_test_reports_like_python() {
    let e = env(&[("OPENROUTER_API_KEY", "k")]);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let ok = Scripted(vec![Ok(("leave".into(), 0.91)), Ok(("none".into(), 0.95))], AtomicUsize::new(0));
    let code = run_intent_test_with(&pairs(&[("leave", "bye"), ("stay", "leave the tab open")]), Some(&e), Some(&ok),
                                    &mut out, &mut err);
    assert_eq!(code, 0);
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "  ok  'bye': leave (0.91), wanted leave\n  ok  'leave the tab open': none (0.95), wanted none\n\
         intent-test ok (gpt-5.4-nano, 2 phrase(s))\n"
    );
    assert!(err.is_empty());

    let (mut out, mut err) = (Vec::new(), Vec::new());
    let bad = Scripted(vec![Ok(("leave".into(), 0.5)), Err(anyhow::anyhow!("boom"))], AtomicUsize::new(0));
    let code = run_intent_test_with(&pairs(&[("leave", "I'm off"), ("mute", "hush")]),
                                    Some(&env(&[("OPENROUTER_API_KEY", "k"), ("EVE_INTENT_MODEL", "m2")])),
                                    Some(&bad), &mut out, &mut err);
    assert_eq!(code, 1);
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "  BAD \"I'm off\": leave (0.50), wanted leave\n  ERROR 'hush': Exception: boom\n"
    );
    assert_eq!(String::from_utf8(err).unwrap(), "intent-test: 2 wrong (m2)\n");

    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(run_intent_test_with(&pairs(&[]), Some(&env(&[])), Some(&ok), &mut out, &mut err), 2);
    assert_eq!(String::from_utf8(err).unwrap(), "intent-test: OPENROUTER_API_KEY not set\n");
}

#[test]
fn error_names_follow_the_sdk() {
    let status = anyhow::Error::from(eve_openrouter::Error::Status {
        status: 503,
        body: "down".into(),
        headers: Box::default(),
    });
    assert_eq!(error_name(&status), "InternalServerError");
    assert_eq!(error_text(&status), "Error code: 503 - down");
    let rate = anyhow::Error::from(eve_openrouter::Error::Status { status: 429, body: String::new(), headers: Box::default() });
    assert_eq!(error_name(&rate), "RateLimitError");
    let parse: anyhow::Error = parse_intent("nope").unwrap_err().into();
    assert_eq!(error_name(&parse), "JSONDecodeError");
}

#[tokio::test]
async fn a_classify_through_the_real_client_reaches_a_local_server() {
    // The whole path on a runtime: the blocking classify runs on a worker thread and drives the async client there.
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 65536];
        let mut got = Vec::new();
        loop {
            let n = sock.read(&mut buf).await.unwrap();
            got.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&got).to_string();
            if let Some(head_end) = text.find("\r\n\r\n") {
                let len = text[..head_end]
                    .lines()
                    .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap()))
                    .unwrap_or(0);
                if got.len() >= head_end + 4 + len {
                    break;
                }
            }
            if n == 0 {
                break;
            }
        }
        let body = r#"{"output":[{"type":"message","content":[{"type":"output_text","text":"{\"intent\":\"leave\",\"confidence\":0.8}"}]}],"usage":{"input_tokens":10,"output_tokens":2}}"#;
        let resp = format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
        sock.write_all(resp.as_bytes()).await.unwrap();
        String::from_utf8_lossy(&got).to_string()
    });
    let dir = tempfile::tempdir().unwrap();
    let opts = eve_openrouter::ClientOptions::default()
        .timeout(timeout())
        .max_retries(0)
        .base_url(format!("http://{addr}"));
    let client = eve_openrouter::async_client(Some("sk-test"), opts).unwrap();
    let c = OpenAIIntentClient::with_client("gpt-5.4-nano", Box::new(client)).with_ledger(eve_spend::Ledger::at(dir.path()));
    let classify = make_classifier(Arc::new(c), timeout(), MIN_CONFIDENCE);
    assert_eq!(classify("alright, I'm heading out".into()).await.as_deref(), Some(LEAVE));
    let request = server.await.unwrap();
    assert!(request.starts_with("POST /responses"), "{request}");
    assert!(request.contains(r#""model":"openai/gpt-5.4-nano""#), "{request}");
    assert!(request.contains(r#""store":false"#));
}
