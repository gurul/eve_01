//! Port of bridge/tests/test_link_memory.py: the Memory card over a fake memory.Memory. Every action, the off state,
//! and that nothing is forgotten without the preview's token. The words here are made up. Also the Memory card's
//! part of test_link_chat.py::test_controls_list_what_this_mac_has (listed only while its memory is built).

use std::collections::HashMap;
use std::io::Write;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use eve_link_memory::{self as link_memory, MemoryApi, MemoryPanel};
use serde_json::{json, Value};

type Calls = Arc<Mutex<Vec<(String, Option<String>)>>>;

/// `FakeMemory`; with `hiding` it is `HidingMemory`. The plain fake has no hide/unhide/hidden, so calling them
/// raises (AttributeError in the Python).
#[derive(Default)]
struct FakeMemory {
    calls: Calls,
    pending: Mutex<HashMap<String, String>>,
    hiding: bool,
    hidden_rows: Mutex<Vec<String>>,
}

impl FakeMemory {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn hiding() -> Arc<Self> {
        Arc::new(Self { hiding: true, ..Self::default() })
    }

    fn call(&self, name: &str, arg: Option<&str>) {
        self.calls.lock().unwrap().push((name.to_string(), arg.map(str::to_string)));
    }

    fn calls(&self) -> Vec<(String, Option<String>)> {
        self.calls.lock().unwrap().clone()
    }

    fn last_arg(&self) -> String {
        self.calls().last().and_then(|c| c.1.clone()).unwrap_or_default()
    }

    fn no_hiding(&self, name: &str) -> anyhow::Error {
        anyhow!("AttributeError: 'FakeMemory' object has no attribute '{name}'")
    }
}

fn call(name: &str, arg: Option<&str>) -> (String, Option<String>) {
    (name.to_string(), arg.map(str::to_string))
}

impl MemoryApi for FakeMemory {
    fn profile(&self) -> Result<Option<String>> {
        self.call("profile", None);
        Ok(Some("# You\n- likes tea".into()))
    }

    fn search(&self, query: &str) -> Result<Value> {
        self.call("search", Some(query));
        if query == "nothing" {
            return Ok(json!({"ok": true, "hits": [], "recalled": [], "said": [], "note": "nothing matched"}));
        }
        Ok(json!({"ok": true, "hits": [{"id": "ana", "line": "- likes tea (2026-10-01)", "src": "2026-10-01 09:00"}],
                  "recalled": ["(2026-10-01, app) likes tea"],
                  "said": [{"day": "2026-10-01", "time": "09:00", "ch": "telegram", "who": "owner", "text": "i like tea",
                            "before": "hi", "after": "ok"}]}))
    }

    fn forget_preview(&self, query: &str) -> Result<Value> {
        self.call("preview", Some(query));
        if query == "nothing" {
            return Ok(json!({"ok": true, "total": 0, "layers": {}, "token": "", "expires_in": 0,
                             "note": "nothing in memory matches"}));
        }
        self.pending.lock().unwrap().insert("tok1".into(), query.into());
        Ok(json!({"ok": true, "total": 3, "layers": {"records": 1, "transcripts": 2, "index": 0}, "token": "tok1",
                  "expires_in": 600}))
    }

    fn forget_apply(&self, token: &str) -> Result<Value> {
        self.call("apply", Some(token));
        if self.pending.lock().unwrap().remove(token).is_none() {
            return Ok(json!({"ok": false, "reason": "the token is unknown, used or expired; preview again"}));
        }
        Ok(json!({"ok": true, "total": 3, "layers": {"records": 1, "transcripts": 2, "index": 0}}))
    }

    fn star(&self, text: &str) -> Result<Option<String>> {
        self.call("star", Some(text));
        Ok(if text == "refuse me" { None } else { Some(format!("★ {text}")) })
    }

    fn hide(&self, text: &str) -> Result<Option<String>> {
        if !self.hiding {
            return Err(self.no_hiding("hide"));
        }
        self.call("hide", Some(text));
        if text == "refuse me" {
            return Ok(None);
        }
        self.hidden_rows.lock().unwrap().push(text.to_string());
        Ok(Some(format!("- {text} (2026-10-09)")))
    }

    fn unhide(&self, text: &str) -> Result<bool> {
        if !self.hiding {
            return Err(self.no_hiding("unhide"));
        }
        self.call("unhide", Some(text));
        let mut rows = self.hidden_rows.lock().unwrap();
        if let Some(i) = rows.iter().position(|r| r == text) {
            rows.remove(i);
            return Ok(true);
        }
        Ok(false)
    }

    fn hidden(&self) -> Result<Vec<String>> {
        if !self.hiding {
            return Err(self.no_hiding("hidden"));
        }
        self.call("hidden", None);
        Ok(self.hidden_rows.lock().unwrap().clone())
    }
}

fn panel(memory: Option<Arc<FakeMemory>>) -> MemoryPanel {
    MemoryPanel::new(move || memory.clone().map(|m| m as Arc<dyn MemoryApi>))
}

fn run<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(fut)
}

fn ask(p: &MemoryPanel, body: Value) -> Value {
    run(p.panel(&body)).expect("the panel answers")
}

#[test]
fn test_off_memory_is_unavailable_and_says_so() {
    let p = panel(None);
    assert!(!p.available());
    assert_eq!(ask(&p, json!({"action": "search", "query": "tea"})), json!({"ok": false, "line": link_memory::OFF_LINE}));
}

#[test]
fn test_the_profile_is_the_default_action() {
    let m = FakeMemory::new();
    let p = panel(Some(m.clone()));
    assert!(p.available());
    let out = ask(&p, json!({}));
    assert_eq!(out, json!({"ok": true, "profile": "# You\n- likes tea", "line": ""}));
    // key order as the Python dict writes it
    assert_eq!(serde_json::to_string(&out).unwrap(), r##"{"ok":true,"profile":"# You\n- likes tea","line":""}"##);
    assert_eq!(m.calls(), vec![call("profile", None)]);
}

#[test]
fn test_a_search_returns_the_three_groups_without_the_lines_around_a_hit() {
    let m = FakeMemory::new();
    let out = ask(&panel(Some(m.clone())), json!({"action": "search", "query": "  tea   please "}));
    assert_eq!(out["ok"], json!(true));
    assert_eq!(out["hits"][0]["id"], json!("ana"));
    assert_eq!(out["recalled"], json!(["(2026-10-01, app) likes tea"]));
    assert_eq!(
        out["said"],
        json!([{"day": "2026-10-01", "time": "09:00", "ch": "telegram", "who": "owner", "text": "i like tea"}])
    );
    assert_eq!(m.calls(), vec![call("search", Some("tea please"))]); // trimmed, one space
    let empty = ask(&panel(Some(m.clone())), json!({"action": "search", "query": "nothing"}));
    assert_eq!(empty["ok"], json!(true));
    assert_eq!(empty["line"], json!("nothing matched"));
    assert_eq!(empty["hits"], json!([]));
    assert_eq!(
        ask(&panel(Some(m)), json!({"action": "search", "query": "   "})),
        json!({"ok": false, "line": "Type a word or two to search for."})
    );
}

#[test]
fn test_forget_is_a_preview_with_counts_then_the_token_once() {
    let m = FakeMemory::new();
    let p = panel(Some(m.clone()));
    let preview = ask(&p, json!({"action": "forget", "query": "tea"}));
    assert_eq!(preview["ok"], json!(true));
    assert_eq!(preview["total"], json!(3));
    assert_eq!(preview["token"], json!("tok1"));
    assert_eq!(preview["line"], json!("3 things match, in facts, what was said"));
    assert!(!preview["line"].as_str().unwrap().contains("tea")); // counts, never the words
    assert!(!m.calls().contains(&call("apply", Some("tok1")))); // nothing forgotten yet
    let done = ask(&p, json!({"action": "apply", "token": "tok1"}));
    assert_eq!(
        done,
        json!({"ok": true, "total": 3, "layers": {"records": 1, "transcripts": 2, "index": 0}, "line": "Forgot 3 things."})
    );
    let again = ask(&p, json!({"action": "apply", "token": "tok1"}));
    assert_eq!(again["ok"], json!(false));
    assert!(again["line"].as_str().unwrap().contains("preview again"));
    let none = ask(&p, json!({"action": "forget", "query": "nothing"}));
    assert_eq!(none["ok"], json!(true));
    assert_eq!(none["total"], json!(0));
    assert_eq!(none["token"], json!(""));
    assert_eq!(none["line"], json!("nothing in memory matches"));
}

#[test]
fn test_remember_stars_one_claim_and_says_so() {
    let m = FakeMemory::new();
    let p = panel(Some(m.clone()));
    assert_eq!(
        ask(&p, json!({"action": "remember", "text": "  Guru was born in Tenali  "})),
        json!({"ok": true, "line": "Remembered."})
    );
    assert_eq!(m.calls(), vec![call("star", Some("Guru was born in Tenali"))]);
    assert_eq!(ask(&p, json!({"action": "remember", "text": "no"}))["ok"], json!(false)); // too short to be a claim
    assert_eq!(
        ask(&p, json!({"action": "remember", "text": "refuse me"})),
        json!({"ok": false, "line": "That couldn't be kept just now."})
    );
    ask(&p, json!({"action": "remember", "text": "x".repeat(2000)}));
    assert_eq!(m.last_arg().chars().count(), link_memory::MAX_REMEMBER_CHARS);
}

#[test]
fn test_an_unknown_action_and_an_overlong_query_are_bounded() {
    let m = FakeMemory::new();
    assert_eq!(ask(&panel(Some(m.clone())), json!({"action": "delete_everything"}))["ok"], json!(false));
    ask(&panel(Some(m.clone())), json!({"action": "search", "query": "x".repeat(1000)}));
    assert_eq!(m.last_arg().chars().count(), link_memory::MAX_QUERY_CHARS);
}

/// A writer the test subscriber logs into, standing in for pytest's caplog.
#[derive(Clone, Default)]
struct LogBuf(Arc<Mutex<Vec<u8>>>);

impl Write for LogBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl LogBuf {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

#[test]
fn test_hide_unhide_and_the_hidden_list() {
    let m = FakeMemory::hiding();
    let p = panel(Some(m.clone()));
    let logs = LogBuf::default();
    let writer = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        assert_eq!(
            ask(&p, json!({"action": "hide", "text": "  likes\n tea  "})),
            json!({"ok": true, "line": "eve won't bring that up again."})
        );
        assert_eq!(ask(&p, json!({"action": "hidden"})), json!({"ok": true, "hidden": ["likes tea"], "line": ""}));
        assert_eq!(
            ask(&p, json!({"action": "unhide", "text": "likes tea"})),
            json!({"ok": true, "line": "eve can bring that up again."})
        );
        assert_eq!(ask(&p, json!({"action": "unhide", "text": "likes tea"}))["ok"], json!(false));
        assert_eq!(ask(&p, json!({"action": "hidden"}))["hidden"], json!([]));
        assert_eq!(ask(&p, json!({"action": "hide", "text": "refuse me"}))["ok"], json!(false));
    });
    assert!(m.calls().contains(&call("hide", Some("likes tea"))));
    let text = logs.text();
    assert!(text.contains("hide → kept") && text.contains("unhide → done"), "{text}");
    assert!(!text.contains("tea"), "{text}"); // logs carry counts, never the words
}

#[test]
fn test_hide_needs_words_and_is_bounded() {
    let m = FakeMemory::hiding();
    let p = panel(Some(m.clone()));
    assert_eq!(ask(&p, json!({"action": "hide", "text": "   "})), json!({"ok": false, "line": "Nothing to hide."}));
    assert_eq!(ask(&p, json!({"action": "unhide"})), json!({"ok": false, "line": "Nothing to unhide."}));
    assert!(!m.calls().iter().any(|c| c.0 == "hide" || c.0 == "unhide"));
    ask(&p, json!({"action": "hide", "text": "x".repeat(2000)}));
    assert_eq!(m.last_arg().chars().count(), link_memory::MAX_HIDE_CHARS);
}

// test_link_chat.py::test_controls_list_what_this_mac_has, the Memory card's part: `MemoryPanel(lambda: None)` is
// not listed and `MemoryPanel(lambda: object())` is.
#[test]
fn test_controls_list_memory_only_while_built() {
    assert!(!panel(None).available());
    assert!(panel(Some(FakeMemory::new())).available());
}

// Beyond the Python tests: the parity edges of the body and the memory's answers.

#[test]
fn a_getter_that_fails_is_unavailable_and_the_panel_raises() {
    let p = MemoryPanel::with_getter(Arc::new(|| Err(anyhow!("RuntimeError: not built"))));
    assert!(!p.available());
    assert!(run(p.panel(&json!({}))).is_err());
}

#[test]
fn a_falsy_action_is_the_profile_and_a_non_string_one_is_unknown() {
    let m = FakeMemory::new();
    let p = panel(Some(m.clone()));
    for action in [json!(null), json!(""), json!(0), json!(false), json!([])] {
        assert_eq!(ask(&p, json!({"action": action}))["profile"], json!("# You\n- likes tea"));
    }
    for action in [json!(1), json!(true), json!(["search"]), json!("Search")] {
        assert_eq!(
            ask(&p, json!({"action": action})),
            json!({"ok": false, "line": "That isn't something the Memory card does."})
        );
    }
}

#[test]
fn a_memory_without_hiding_raises_out_of_the_panel() {
    let p = panel(Some(FakeMemory::new()));
    assert!(run(p.panel(&json!({"action": "hidden"}))).is_err());
    assert!(run(p.panel(&json!({"action": "hide", "text": "likes tea"}))).is_err());
}

#[test]
fn the_token_is_stripped_and_cut_and_a_number_reads_as_its_str() {
    let m = FakeMemory::new();
    let p = panel(Some(m.clone()));
    ask(&p, json!({"action": "apply", "token": format!("  {}  ", "t".repeat(100))}));
    assert_eq!(m.last_arg(), "t".repeat(link_memory::MAX_TOKEN_CHARS));
    ask(&p, json!({"action": "search", "query": 1.5}));
    assert_eq!(m.last_arg(), "1.5");
}

struct Answering(Value);

impl MemoryApi for Answering {
    fn profile(&self) -> Result<Option<String>> {
        Ok(None)
    }
    fn search(&self, _: &str) -> Result<Value> {
        Ok(self.0.clone())
    }
    fn forget_preview(&self, _: &str) -> Result<Value> {
        Ok(self.0.clone())
    }
    fn forget_apply(&self, _: &str) -> Result<Value> {
        Ok(self.0.clone())
    }
    fn star(&self, _: &str) -> Result<Option<String>> {
        Ok(Some(String::new()))
    }
    fn hide(&self, _: &str) -> Result<Option<String>> {
        Ok(None)
    }
    fn unhide(&self, _: &str) -> Result<bool> {
        Ok(false)
    }
    fn hidden(&self) -> Result<Vec<String>> {
        Ok(vec![])
    }
}

fn answering(v: Value) -> MemoryPanel {
    let m: Arc<dyn MemoryApi> = Arc::new(Answering(v));
    MemoryPanel::new(move || Some(m.clone()))
}

#[test]
fn the_memorys_odd_answers_read_as_the_python_reads_them() {
    // no profile yet
    assert_eq!(
        ask(&answering(json!({})), json!({"action": "profile"})),
        json!({"ok": true, "profile": "", "line": "eve has no profile of you yet."})
    );
    // an empty starred line is not kept
    assert_eq!(ask(&answering(json!({})), json!({"action": "remember", "text": "long enough"}))["ok"], json!(false));
    // a search without "ok" went fine; a failed one says its reason, or the default
    let fine = ask(&answering(json!({"said": [{"day": "d"}, "x"], "recalled": [1, null], "note": 0})),
                   json!({"action": "search", "query": "q"}));
    assert_eq!(
        fine,
        json!({"ok": true, "hits": [], "recalled": ["1", "None"],
               "said": [{"day": "d", "time": "", "ch": "", "who": "", "text": ""}], "line": ""})
    );
    assert_eq!(
        ask(&answering(json!({"ok": false})), json!({"action": "search", "query": "q"})),
        json!({"ok": false, "line": "That search didn't work."})
    );
    assert_eq!(
        ask(&answering(json!({"ok": 0, "reason": "index down"})), json!({"action": "forget", "query": "q"})),
        json!({"ok": false, "line": "index down"})
    );
    // counts as strings, one thing, layers that are not a dict
    let one = ask(&answering(json!({"ok": true, "total": "1", "layers": [["chat_log", 1]], "token": "t",
                                    "expires_in": 600.0})),
                  json!({"action": "forget", "query": "q"}));
    assert_eq!(
        serde_json::to_string(&one).unwrap(),
        r#"{"ok":true,"total":1,"layers":{"chat_log":1},"token":"t","expires_in":600,"line":"1 thing match, in memory"}"#
    );
    let unnamed = ask(&answering(json!({"ok": true, "total": 2, "layers": {"meetings": 1, "other": 1, "archive": 0}})),
                      json!({"action": "forget", "query": "q"}));
    assert_eq!(unnamed["line"], json!("2 things match, in meetings, other"));
    // apply: nothing to forget, one thing, a failure without a reason
    assert_eq!(
        ask(&answering(json!({"ok": true, "total": 0})), json!({"action": "apply", "token": "t"})),
        json!({"ok": true, "total": 0, "layers": {}, "line": "Nothing to forget."})
    );
    assert_eq!(
        ask(&answering(json!({"ok": true, "total": 1, "layers": {"records": 1}})), json!({"action": "apply"}))["line"],
        json!("Forgot 1 thing.")
    );
    assert_eq!(
        ask(&answering(json!({"total": 2})), json!({"action": "apply", "token": "t"})),
        json!({"ok": false, "total": 2, "layers": {}, "line": "That didn't work."})
    );
    // a count that is not a number raises, as int() does
    assert!(run(answering(json!({"ok": true, "total": "many"})).panel(&json!({"action": "apply"}))).is_err());
    assert!(run(answering(json!("not a dict")).panel(&json!({"action": "search", "query": "q"}))).is_err());
}

#[test]
fn layers_names_what_a_forget_touches() {
    assert_eq!(link_memory::layers(None), "memory");
    assert_eq!(link_memory::layers(Some(&json!([1]))), "memory");
    assert_eq!(link_memory::layers(Some(&json!({"records": 0}))), "memory");
    assert_eq!(
        link_memory::layers(Some(&json!({"index": 2, "records": 1, "transcripts": 1, "archive": true}))),
        "meanings, facts, what was said, notes"
    );
}
