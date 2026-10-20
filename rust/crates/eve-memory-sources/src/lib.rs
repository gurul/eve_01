//! Sources under a reply: which memory lines a turn's answer leaned on (docs/app-chat.md, "Sources").
//!
//! ChatGPT's memory article: "When ChatGPT uses relevant personal context, Sources may appear below the response."
//! eve does the same in the Eve Link app. While a turn runs, every memory read the brain makes (`memory_search`,
//! `memory_read`) is looked at after it returns, and what it found is kept as a short list of sources: a record's
//! fact line, a recalled meaning, or a line that was said. When the turn's reply is written to the transcript the list
//! rides on that one line as `src`; `/api/link/messages` gives it to the app as `sources` (link_chat._row), and
//! the app shows "Sources · N" under the reply.
//!
//! The model never sees any of this: the tool result it is handed is the one the Memory returned, untouched, and the
//! history is the chat log's words. Only the transcript line and the app's row carry the sources.
//!
//! A source is `{"kind": "fact" | "remembered" | "said", "text": str, "when"?: str}`, at most [`MAX_SOURCES`] of
//! them, each text at most [`MAX_TEXT_CHARS`].
//!
//! Port of `bridge/src/eve_bridge/memory_sources.py`. Inputs are free-form JSON (`serde_json::Value`), as the
//! Python takes any object; every place the Python calls `str(x or "")` goes through [`py_str_or_empty`], which
//! renders a value the way Python's `str()` renders the same decoded JSON.

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The kinds a source may have, in the Python's tuple order.
pub const KINDS: [&str; 3] = ["fact", "remembered", "said"];
pub const MAX_SOURCES: usize = 6;
pub const MAX_TEXT_CHARS: usize = 200;
pub const MAX_WHEN_CHARS: usize = 40;

/// One source: `{"kind", "text", "when"?}`. `when` is left out of the JSON when absent, as the Python dict has no
/// such key then. Field order matches the Python dict's insertion order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub kind: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
}

impl Source {
    /// The source as the JSON object the Python writes (`{"kind", "text", "when"?}`).
    pub fn to_value(&self) -> Value {
        let mut m = serde_json::Map::new();
        m.insert("kind".into(), Value::String(self.kind.clone()));
        m.insert("text".into(), Value::String(self.text.clone()));
        if let Some(w) = &self.when {
            m.insert("when".into(), Value::String(w.clone()));
        }
        Value::Object(m)
    }
}

/// A list of sources as a JSON array (what rides on a transcript line as `src`).
pub fn to_value(sources: &[Source]) -> Value {
    Value::Array(sources.iter().map(Source::to_value).collect())
}

// memory._recalled's "(day time, channel) text". Python's `\s` on str also matches \x1c-\x1f.
static RECALLED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)^\((?P<tag>[^()]{1,80})\)[\s\x1c-\x1f]*(?P<text>.+)$").expect("RECALLED pattern")
});

fn who_name(who: &str) -> &str {
    match who {
        "owner" => "You",
        "eve" => "eve",
        "claude" => "Claude",
        "notes" => "Notes",
        other => other,
    }
}

// ---- Python str() semantics over decoded JSON ---------------------------------------------------------------

/// Python's `str.isspace()` for one char: Unicode White_Space plus the separators \x1c-\x1f.
fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Python truthiness of a decoded JSON value.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Python's `repr(float)`: shortest round-trip digits, scientific below 1e-4 or from 1e16 up.
fn py_float(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    let sci = format!("{f:e}");
    let (mant, exp) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    if (-4..16).contains(&exp) {
        let s = format!("{f}");
        if s.contains('.') {
            s
        } else {
            format!("{s}.0")
        }
    } else {
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{mant}e{sign}{:02}", exp.abs())
    }
}

/// Whether Python's `str.isprintable()` holds for one char, for the chars JSON text can carry in practice:
/// control chars (Cc), the format chars (Cf) in common use, and separators other than the plain space are not.
fn py_printable(c: char) -> bool {
    if c == ' ' {
        return true;
    }
    if c.is_control() || c.is_whitespace() {
        return false;
    }
    !matches!(
        c,
        '\u{ad}'
            | '\u{600}'..='\u{605}'
            | '\u{61c}'
            | '\u{6dd}'
            | '\u{70f}'
            | '\u{180e}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206f}'
            | '\u{e000}'..='\u{f8ff}'
            | '\u{feff}'
            | '\u{fff9}'..='\u{fffb}'
            | '\u{f0000}'..='\u{10ffff}'
    )
}

/// Python's `repr(str)`.
fn py_repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if !py_printable(c) => {
                let n = c as u32;
                if n < 0x100 {
                    out.push_str(&format!("\\x{n:02x}"));
                } else if n < 0x10000 {
                    out.push_str(&format!("\\u{n:04x}"));
                } else {
                    out.push_str(&format!("\\U{n:08x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python's `repr()` of a decoded JSON value.
fn py_repr(v: &Value) -> String {
    match v {
        Value::String(s) => py_repr_str(s),
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter()
                .map(|(k, v)| format!("{}: {}", py_repr_str(k), py_repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => py_str(other),
    }
}

/// Python's `str()` of a decoded JSON value.
pub fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(u) = n.as_u64() {
                u.to_string()
            } else {
                py_float(n.as_f64().unwrap_or(0.0))
            }
        }
        Value::String(s) => s.clone(),
        other => py_repr(other),
    }
}

/// Python's `str(v or "")`: "" for any falsy value (None, False, 0, "", [], {}), else `str(v)`.
pub fn py_str_or_empty(v: &Value) -> String {
    if truthy(v) {
        py_str(v)
    } else {
        String::new()
    }
}

/// Python's `a or b` over decoded JSON values.
fn py_or<'a>(a: &'a Value, b: &'a Value) -> &'a Value {
    if truthy(a) {
        a
    } else {
        b
    }
}

/// `d.get(key)` on a JSON object, `None` (Null) when missing.
fn get<'a>(d: &'a serde_json::Map<String, Value>, key: &str) -> &'a Value {
    static NULL: Value = Value::Null;
    d.get(key).unwrap_or(&NULL)
}

// ---- the module -----------------------------------------------------------------------------------------------

/// `_clip`: whitespace runs collapsed to one space, then cut to `cap` chars with an ellipsis.
fn clip(text: &str, cap: usize) -> String {
    let s = text.split(py_space).filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" ");
    if s.chars().count() <= cap {
        return s;
    }
    let head: String = s.chars().take(cap.saturating_sub(1)).collect();
    format!("{}…", head.trim_end_matches(py_space))
}

/// `_source`: a source of `kind` from text and when (both already `str(x or "")`), or None when the kind is not
/// one of [`KINDS`] or the text is blank.
fn source(kind: &str, text: &str, when: &str) -> Option<Source> {
    let body = clip(text, MAX_TEXT_CHARS);
    if !KINDS.contains(&kind) || body.is_empty() {
        return None;
    }
    let w = clip(when, MAX_WHEN_CHARS);
    Some(Source { kind: kind.to_string(), text: body, when: (!w.is_empty()).then_some(w) })
}

/// `_said`: a line said (`{day, time, who, text}`), its speaker named the way the app shows it.
fn said(hit: &serde_json::Map<String, Value>) -> Option<Source> {
    let who = py_str_or_empty(get(hit, "who"));
    let mut text = py_str_or_empty(get(hit, "text"));
    if !who.is_empty() {
        text = format!("{}: {}", who_name(&who), text);
    }
    let when = [py_str_or_empty(get(hit, "day")), py_str_or_empty(get(hit, "time"))]
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    source("said", &text, &when)
}

/// `_hit`: one entry of a search's `hits`: a record's fact (`{id, line, src?}`) or, from a bare transcript search,
/// a line said (`{day, time, who, text}`).
fn hit(hit: &Value) -> Option<Source> {
    let Value::Object(h) = hit else {
        return None;
    };
    if h.contains_key("line") {
        let rid = py_str_or_empty(get(h, "id"));
        let line = py_str_or_empty(get(h, "line"));
        let text = if rid.is_empty() { line } else { format!("{rid}: {line}") };
        return source("fact", &text, &py_str_or_empty(get(h, "src")));
    }
    if h.contains_key("text") {
        return said(h);
    }
    None
}

/// `_recalled`: a recalled meaning, either a dict (`{text, src?, day?}`) or memory._recalled's
/// "(day time, channel) text" string.
fn recalled(row: &Value) -> Option<Source> {
    if let Value::Object(r) = row {
        let when = py_or(get(r, "src"), get(r, "day"));
        return source("remembered", &py_str_or_empty(get(r, "text")), &py_str_or_empty(when));
    }
    let raw = py_str_or_empty(row);
    let stripped = raw.trim_matches(py_space);
    match RECALLED.captures(stripped) {
        Some(m) => source("remembered", &m["text"], &m["tag"]),
        None => source("remembered", &raw, ""),
    }
}

/// `_list`: the value as a list, or empty when it is not one.
fn list(raw: &Value) -> &[Value] {
    match raw {
        Value::Array(a) => a.as_slice(),
        _ => &[],
    }
}

/// What one memory read found, as sources, in the order shown (taken in turn from the facts, the meanings and
/// the lines said, so one kind never crowds out the rest). [] for anything else, a failed read, or nothing found.
pub fn from_result(name: &str, result: &Value) -> Vec<Source> {
    let Value::Object(r) = result else {
        return Vec::new();
    };
    if matches!(r.get("ok"), Some(Value::Bool(false))) {
        return Vec::new();
    }
    if name == "memory_read" {
        let src = if let Some(Value::String(record)) = r.get("record") {
            source("fact", record, "")
        } else if let Some(Value::String(text)) = r.get("text") {
            source("said", text, &py_str_or_empty(get(r, "day")))
        } else {
            None
        };
        return src.into_iter().collect();
    }
    if name != "memory_search" {
        return Vec::new();
    }
    let groups: [Vec<Source>; 3] = [
        list(get(r, "hits")).iter().filter_map(hit).collect(),
        list(get(r, "recalled")).iter().filter_map(recalled).collect(),
        list(get(r, "said")).iter().filter_map(|h| h.as_object().and_then(said)).collect(),
    ];
    let mut out: Vec<Source> = Vec::new();
    let mut i = 0;
    while out.len() < MAX_SOURCES && groups.iter().any(|g| i < g.len()) {
        for g in &groups {
            if i < g.len() && out.len() < MAX_SOURCES {
                out.push(g[i].clone());
            }
        }
        i += 1;
    }
    out
}

/// `found` onto a turn's `kept` sources: no duplicate (same kind and text), at most [`MAX_SOURCES`].
pub fn add(kept: &mut Vec<Source>, found: &[Source]) {
    let mut seen: std::collections::HashSet<(String, String)> =
        kept.iter().map(|s| (s.kind.clone(), s.text.clone())).collect();
    for s in found {
        if kept.len() >= MAX_SOURCES {
            return;
        }
        if seen.insert((s.kind.clone(), s.text.clone())) {
            kept.push(s.clone());
        }
    }
}

/// A transcript line's `src` as the app may show it: well-formed entries only, string fields, capped.
pub fn sanitize(raw: &Value) -> Vec<Source> {
    let Value::Array(items) = raw else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for s in items {
        if out.len() >= MAX_SOURCES {
            break;
        }
        let Value::Object(s) = s else {
            continue;
        };
        let Some(Value::String(kind)) = s.get("kind") else {
            continue;
        };
        if !KINDS.contains(&kind.as_str()) {
            continue;
        }
        let Some(Value::String(text)) = s.get("text") else {
            continue;
        };
        let when = match s.get("when") {
            Some(Value::String(w)) => w.as_str(),
            _ => "",
        };
        if let Some(clean) = source(kind, text, when) {
            out.push(clean);
        }
    }
    out
}

/// Whether any of a line's sources holds words `pred` matches (transcripts' forget scrubs them).
pub fn mentions(raw: &Value, pred: impl Fn(&str) -> bool) -> bool {
    match raw {
        Value::Array(items) => items
            .iter()
            .any(|s| matches!(s, Value::Object(o) if pred(&py_str_or_empty(get(o, "text"))))),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // memory.Memory.search's shape: record facts, recalled meanings, and the words said
    fn search_result() -> Value {
        json!({
            "ok": true,
            "hits": [{"id": "plumber", "line": "comes on Fridays", "src": "2026-09-30"}],
            "recalled": ["(2026-10-01 09:12, voice) the plumber prefers mornings"],
            "said": [{"day": "2026-10-02", "time": "18:40", "ch": "telegram", "who": "owner",
                      "text": "the plumber is booked for Friday", "before": "", "after": ""}],
        })
    }

    fn vals(sources: &[Source]) -> Value {
        to_value(sources)
    }

    #[test]
    fn test_a_search_result_becomes_sources_of_each_kind_in_turn() {
        let got = from_result("memory_search", &search_result());
        assert_eq!(
            vals(&got),
            json!([
                {"kind": "fact", "text": "plumber: comes on Fridays", "when": "2026-09-30"},
                {"kind": "remembered", "text": "the plumber prefers mornings", "when": "2026-10-01 09:12, voice"},
                {"kind": "said", "text": "You: the plumber is booked for Friday", "when": "2026-10-02 18:40"},
            ])
        );
    }

    #[test]
    fn test_a_read_is_a_fact_or_a_stretch_said_and_a_failure_is_nothing() {
        assert_eq!(
            vals(&from_result("memory_read", &json!({"ok": true, "record": "# plumber\n- comes on Fridays"}))),
            json!([{"kind": "fact", "text": "# plumber - comes on Fridays"}])
        );
        assert_eq!(
            vals(&from_result("memory_read", &json!({"ok": true, "day": "2026-10-02", "text": "18:40 Owner: hi"}))),
            json!([{"kind": "said", "text": "18:40 Owner: hi", "when": "2026-10-02"}])
        );
        assert!(from_result("memory_search", &json!({"ok": false, "reason": "x"})).is_empty());
        assert!(from_result("forget_preview", &search_result()).is_empty());
        assert!(from_result("memory_search", &json!("not a dict")).is_empty());
    }

    #[test]
    fn test_sources_are_capped_in_number_and_length_and_never_repeat() {
        let hits: Vec<Value> = (0..10).map(|i| json!({"id": format!("r{i}"), "line": "x".repeat(500)})).collect();
        let many = json!({"ok": true, "hits": hits});
        let got = from_result("memory_search", &many);
        assert_eq!(got.len(), MAX_SOURCES);
        assert!(got.iter().all(|s| s.text.chars().count() <= MAX_TEXT_CHARS));
        let mut kept: Vec<Source> = Vec::new();
        add(&mut kept, &got[..2]);
        add(&mut kept, &got[..3]);
        assert_eq!(kept, got[..3].to_vec());
    }

    #[test]
    fn test_sanitize_keeps_well_formed_entries_only() {
        let mut raw = vec![
            json!({"kind": "fact", "text": "a"}),
            json!({"kind": "evil", "text": "b"}),
            json!({"kind": "said", "text": 3}),
            json!("nope"),
            json!({"kind": "said", "text": "c", "when": 5}),
            json!({"kind": "remembered", "text": " ", "when": "x"}),
        ];
        raw.extend((0..10).map(|i| json!({"kind": "fact", "text": format!("n{i}"), "when": "w".repeat(99)})));
        let got = sanitize(&Value::Array(raw));
        assert_eq!(vals(&got[..2]), json!([{"kind": "fact", "text": "a"}, {"kind": "said", "text": "c"}]));
        assert_eq!(got.len(), MAX_SOURCES);
        assert!(got.iter().all(|s| s.when.as_deref().unwrap_or("").chars().count() <= MAX_WHEN_CHARS));
        assert!(sanitize(&json!("x")).is_empty() && sanitize(&Value::Null).is_empty());
    }

    // ---- beyond the Python tests: the edges of the same functions ------------------------------------------

    #[test]
    fn test_clip_collapses_whitespace_and_cuts_with_an_ellipsis() {
        assert_eq!(clip("  a \n\t b\u{a0}c\u{1f}d  ", 200), "a b c d");
        // 199 chars then "…", trailing space before the cut dropped
        let s = format!("{} {}", "y".repeat(198), "z".repeat(10));
        let got = clip(&s, 200);
        assert_eq!(got, format!("{}…", "y".repeat(198)));
        assert_eq!(clip(&"é".repeat(200), 200).chars().count(), 200);
        assert_eq!(clip(&"é".repeat(201), 200), format!("{}…", "é".repeat(199)));
    }

    #[test]
    fn test_said_names_the_speaker_and_joins_day_and_time() {
        let r = json!({"ok": true, "said": [
            {"who": "eve", "text": "a", "day": "d"},
            {"who": "claude", "text": "b", "time": "t"},
            {"who": "notes", "text": "c"},
            {"who": "stranger", "text": "d"},
            {"text": "e"},
            "not a dict",
        ]});
        assert_eq!(
            vals(&from_result("memory_search", &r)),
            json!([
                {"kind": "said", "text": "eve: a", "when": "d"},
                {"kind": "said", "text": "Claude: b", "when": "t"},
                {"kind": "said", "text": "Notes: c"},
                {"kind": "said", "text": "stranger: d"},
                {"kind": "said", "text": "e"},
            ])
        );
    }

    #[test]
    fn test_hits_take_facts_without_ids_and_lines_said() {
        let r = json!({"hits": [
            {"line": "bare line"},
            {"id": "", "line": "no id", "src": ""},
            {"day": "2026-10-02", "time": "18:40", "who": "owner", "text": "hi"},
            {"other": 1},
            7,
        ]});
        assert_eq!(
            vals(&from_result("memory_search", &r)),
            json!([
                {"kind": "fact", "text": "bare line"},
                {"kind": "fact", "text": "no id"},
                {"kind": "said", "text": "You: hi", "when": "2026-10-02 18:40"},
            ])
        );
    }

    #[test]
    fn test_recalled_takes_dicts_bare_strings_and_tagged_strings() {
        let r = json!({"recalled": [
            {"text": "dict meaning", "src": "", "day": "2026-10-01"},
            {"text": "with src", "src": "s"},
            "  no tag here  ",
            "(tag)   spaced text",
            "(a(b) nested parens",
            "(only tag)",
            "",
        ]});
        assert_eq!(
            vals(&from_result("memory_search", &r)),
            json!([
                {"kind": "remembered", "text": "dict meaning", "when": "2026-10-01"},
                {"kind": "remembered", "text": "with src", "when": "s"},
                {"kind": "remembered", "text": "no tag here"},
                {"kind": "remembered", "text": "spaced text", "when": "tag"},
                {"kind": "remembered", "text": "(a(b) nested parens"},
                {"kind": "remembered", "text": "(only tag)"},
            ])
        );
        // a tag over 80 chars is not a tag
        let long = format!("({}) text", "t".repeat(81));
        assert_eq!(recalled(&json!(long)).map(|s| s.when), Some(None));
    }

    #[test]
    fn test_groups_interleave_so_one_kind_never_crowds_out_the_rest() {
        let hits: Vec<Value> = (0..5).map(|i| json!({"id": format!("h{i}"), "line": "x"})).collect();
        let rec: Vec<Value> = (0..5).map(|i| json!(format!("r{i}"))).collect();
        let r = json!({"hits": hits, "recalled": rec, "said": [{"text": "s0"}]});
        let got: Vec<String> = from_result("memory_search", &r).into_iter().map(|s| s.text).collect();
        assert_eq!(got, ["h0: x", "r0", "s0", "h1: x", "r1", "h2: x"]);
    }

    #[test]
    fn test_a_read_with_neither_record_nor_text_is_nothing() {
        assert!(from_result("memory_read", &json!({"ok": true, "record": 5})).is_empty());
        assert!(from_result("memory_read", &json!({"ok": true, "record": "  "})).is_empty());
        // "ok" must be exactly False to fail: 0 and a missing key still read
        assert_eq!(from_result("memory_read", &json!({"ok": 0, "record": "r"})).len(), 1);
        assert_eq!(from_result("memory_read", &json!({"record": "r"})).len(), 1);
    }

    #[test]
    fn test_non_string_values_render_as_python_str_does() {
        assert_eq!(py_str_or_empty(&json!(0)), "");
        assert_eq!(py_str_or_empty(&json!(false)), "");
        assert_eq!(py_str_or_empty(&json!([])), "");
        assert_eq!(py_str_or_empty(&json!(true)), "True");
        assert_eq!(py_str_or_empty(&json!(12)), "12");
        assert_eq!(py_str_or_empty(&json!(1.0)), "1.0");
        assert_eq!(py_str_or_empty(&json!(0.5)), "0.5");
        assert_eq!(py_str_or_empty(&json!(1e16)), "1e+16");
        assert_eq!(py_str_or_empty(&json!(1.5e-5)), "1.5e-05");
        assert_eq!(py_str_or_empty(&json!(123456.789)), "123456.789");
        assert_eq!(py_str_or_empty(&json!([1, "a", null, true])), "[1, 'a', None, True]");
        assert_eq!(py_str_or_empty(&json!({"k": "it's", "n": "a\"b'c\n"})), r#"{'k': "it's", 'n': 'a"b\'c\n'}"#);
        let r = json!({"hits": [{"id": 7, "line": "x", "src": 2026}]});
        assert_eq!(vals(&from_result("memory_search", &r)), json!([{"kind": "fact", "text": "7: x", "when": "2026"}]));
    }

    #[test]
    fn test_mentions_reads_each_source_text() {
        let raw = json!([{"kind": "said", "text": "the gate code"}, "x", {"kind": "fact"}]);
        assert!(mentions(&raw, |t| t.contains("gate")));
        assert!(!mentions(&raw, |t| t.contains("boat")));
        assert!(mentions(&raw, |t| t.is_empty()));           // a source with no text reads as ""
        assert!(!mentions(&json!("the gate code"), |_| true));
        assert!(!mentions(&Value::Null, |_| true));
    }

    #[test]
    fn test_sources_round_trip_through_json() {
        let s: Vec<Source> = serde_json::from_value(json!([{"kind": "fact", "text": "a"},
                                                          {"kind": "said", "text": "b", "when": "w"}]))
            .unwrap();
        assert_eq!(serde_json::to_value(&s).unwrap(), to_value(&s));
        assert_eq!(serde_json::to_string(&s[0]).unwrap(), r#"{"kind":"fact","text":"a"}"#);
    }
}
