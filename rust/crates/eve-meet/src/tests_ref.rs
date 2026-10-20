//! meet.py's own answers for edge inputs (`tests/fixtures/python_reference.json`, written by
//! `tests/fixtures/gen_reference.py`), and the parts of other Python test files that exercise meet.py:
//! `test_model_memory_roles.py` (the summary request), `test_steel_local.py` (Meet never joins on Steel) and
//! `test_apps_mcp.py` (a compacted calendar event keeps what Meet reads).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::Arc;

use chrono::{FixedOffset, NaiveDate, TimeZone};
use parking_lot::Mutex;
use regex::Regex;
use serde_json::{json, Map, Value};

use crate::*;

fn reference() -> Value {
    let raw = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/python_reference.json"));
    serde_json::from_str(raw).unwrap()
}

fn opt_str(v: &Option<String>) -> Value {
    v.clone().map(Value::String).unwrap_or(Value::Null)
}

#[test]
fn links_and_codes_match_pythons() {
    let r = reference();
    for case in r["links"].as_array().unwrap() {
        let text = case[0].as_str().unwrap();
        let (url, why) = parse_link(text);
        assert_eq!((opt_str(&url), Value::from(why)), (case[1].clone(), case[2].clone()), "{text}");
    }
    for case in r["codes"].as_array().unwrap() {
        assert_eq!(meeting_code(case[0].as_str().unwrap()), case[1].as_str().unwrap());
    }
}

#[test]
fn merges_and_merger_scripts_match_pythons() {
    let r = reference();
    for case in r["merges"].as_array().unwrap() {
        let (a, b) = (case[0].as_str().unwrap(), case[1].as_str().unwrap());
        assert_eq!(opt_str(&merge(a, b)), case[2], "{a:?} / {b:?}");
    }
    for script in r["scripts"].as_array().unwrap() {
        let mut m = CaptionMerger::new();
        let mut got = Vec::new();
        for step in script[0].as_array().unwrap() {
            let lines = match step[0].as_str().unwrap() {
                "feed" => m.feed(&step[1]).unwrap(),
                "tick" => m.tick(step[1].as_f64().unwrap()),
                _ => m.finish(),
            };
            got.push(json!(lines.iter().map(|ln| json!([ln.speaker, ln.text, ln.at])).collect::<Vec<_>>()));
        }
        assert_eq!(Value::Array(got), script[1], "{}", script[0]);
    }
}

fn pt_at(h: u32, m: u32) -> chrono::DateTime<FixedOffset> {
    FixedOffset::west_opt(7 * 3600).unwrap().with_ymd_and_hms(2026, 9, 26, h, m, 0).unwrap()
}

#[test]
fn the_calendar_matches_pythons() {
    let r = reference();
    let events = r["events"].as_array().unwrap().clone();
    let links: Vec<Value> = events.iter().map(|e| opt_str(&event_link(e))).collect();
    assert_eq!(Value::Array(links), r["event_links"]);
    for case in r["events_in"].as_array().unwrap() {
        assert_eq!(events_in(&case[0], 0).len() as u64, case[1].as_u64().unwrap(), "{}", case[0]);
    }
    for case in r["finds"].as_array().unwrap() {
        let words = case[0].as_str().unwrap();
        let now = pt_at(case[1][0].as_u64().unwrap() as u32, case[1][1].as_u64().unwrap() as u32);
        let (e, link, why) = find_meeting(&events, words, now);
        let got = json!([e.map(|e| e["summary"].clone()).unwrap_or(Value::Null), link, why]);
        assert_eq!(got, case[2], "{words} at {now}");
    }
}

fn masked(body: &str) -> String {
    let ended = Regex::new(r"(?m)^ended: \d{4}-\d\d-\d\d \d\d:\d\d ").unwrap();
    ended.replace_all(body, "ended: <now> ").into_owned()
}

#[test]
fn the_notes_file_and_message_match_pythons() {
    let r = reference();
    for case in r["notes"].as_array().unwrap() {
        let d = tempfile::tempdir().unwrap();
        let started = eve_transcripts::py::local_aware(NaiveDate::from_ymd_opt(2026, 9, 26).unwrap().and_hms_opt(19, 0, 0).unwrap());
        let mut f = NotesFile::new(&d.path().join("meetings"), started, "abc-defg-hij").unwrap();
        let lines: Vec<Line> = case["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| Line { speaker: l[0].as_str().unwrap().into(), text: l[1].as_str().unwrap().into(), at: l[2].as_f64().unwrap() })
            .collect();
        f.append(&lines).unwrap();
        let summary: Map<String, Value> = case["summary"].as_object().unwrap().clone();
        let label = case["label"].as_str().unwrap();
        f.finish(&summary, label, "the call ended").unwrap();
        assert_eq!(f.path.file_name().unwrap().to_string_lossy(), case["name"].as_str().unwrap());
        assert_eq!(masked(&std::fs::read_to_string(&f.path).unwrap()), case["body"].as_str().unwrap(), "{summary:?}");
        let message = summary_message(label, "the call ended", &summary, f.lines.len(), Some(&f.path));
        assert_eq!(message, case["message"].as_str().unwrap());
    }
    for case in r["line_texts"].as_array().unwrap() {
        let ln = Line { speaker: case[0].as_str().unwrap().into(), text: case[1].as_str().unwrap().into(), at: case[2].as_f64().unwrap() };
        assert_eq!(line_text(&ln), case[3].as_str().unwrap());
    }
}

#[test]
fn the_configuration_matches_pythons() {
    let r = reference();
    for case in r["configs"].as_array().unwrap() {
        let env: HashMap<String, String> =
            case[0].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string())).collect();
        let c = configured(Some(&env));
        let want = &case[1];
        assert_eq!(c.enabled, want["enabled"].as_bool().unwrap(), "{env:?}");
        assert_eq!(c.profile, want["profile"].as_str().unwrap(), "{env:?}");
        assert_eq!(c.lobby_minutes, want["lobby_minutes"].as_f64().unwrap(), "{env:?}");
        assert_eq!(c.max_hours, want["max_hours"].as_f64().unwrap(), "{env:?}");
        assert_eq!(c.quiet, want["quiet"].as_bool().unwrap(), "{env:?}");
        assert_eq!(c.guest_name, want["guest_name"].as_str().unwrap(), "{env:?}");
    }
    for case in r["g"].as_array().unwrap() {
        assert_eq!(py::fmt_g(case[0].as_f64().unwrap()), case[1].as_str().unwrap());
    }
}

#[test]
fn the_tools_prompts_and_constants_are_pythons() {
    let r = reference();
    assert_eq!(*TOOLS, r["tools"]);
    assert_eq!(json!(TOOL_NAMES), r["tool_names"]);
    assert_eq!(INSTRUCTIONS_BLOCK, r["instructions"].as_str().unwrap());
    assert_eq!(SUMMARY_PROMPT, r["prompt"].as_str().unwrap());
    assert_eq!(*MEET_SCHEMA, r["schema"]);
    assert_eq!(OFF_REASON, r["off_reason"].as_str().unwrap());
    assert_eq!(json!([ABOUT_IN, ABOUT_LOBBY, ABOUT_CAPTIONS, ABOUT_JOINING, ABOUT_NOT_JOINING]), r["abouts"]);
    for case in r["why_not_in"].as_array().unwrap() {
        assert_eq!(why_not_in(case[0].as_object().unwrap()), case[1].as_str().unwrap());
    }
    // the tool key order is Python's dict order, which the model sees
    let first = TOOLS[0].as_object().unwrap().keys().cloned().collect::<Vec<_>>();
    assert_eq!(first, ["type", "name", "strict", "description", "parameters"]);
}

// ---- test_model_memory_roles.py ---------------------------------------------------------------------

/// A local stand-in for OpenRouter: answers every POST with a Responses reply whose text is `{}`, and keeps each
/// request's path and JSON body (the Python test's mock transport).
type Sent = Arc<Mutex<Vec<(String, Value)>>>;

fn fake_openrouter() -> (String, Sent) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let requests: Sent = Arc::default();
    let seen = requests.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut first = String::new();
            if reader.read_line(&mut first).is_err() {
                continue;
            }
            let path = first.split_whitespace().nth(1).unwrap_or("").to_string();
            let mut length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; length];
            let _ = reader.read_exact(&mut body);
            let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let model = parsed.get("model").cloned();
            seen.lock().push((path, parsed));
            let reply = json!({"id": "test", "object": "response", "created_at": 0, "status": "completed",
                               "model": model, "output": [{"id": "message", "type": "message", "role": "assistant",
                               "status": "completed", "content": [{"type": "output_text", "text": "{}",
                                                                   "annotations": []}]}]});
            let payload = serde_json::to_vec(&reply).unwrap();
            let mut stream = stream;
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                payload.len()
            );
            let _ = stream.write_all(&payload);
        }
    });
    (base, requests)
}

#[test]
fn test_meet_summary_preserves_model_override() {
    for (override_, model) in [(None, "openai/gpt-6-luna"), (Some("  "), "openai/gpt-6-luna"), (Some(" gpt-6.1-sol "), "openai/gpt-6.1-sol")] {
        let (base, sent) = fake_openrouter();
        let d = tempfile::tempdir().unwrap();
        let mut env: HashMap<String, String> = HashMap::from([("OPENROUTER_API_KEY".into(), "test-provider-key".into())]);
        if let Some(o) = override_ {
            env.insert("EVE_MEET_SUMMARY_MODEL".into(), o.into());
        }
        let mut s = summarizer(Some(&env), eve_openrouter::ClientOptions::default().base_url(base)).unwrap();
        s.ledger = eve_spend::Ledger::at(d.path());
        assert_eq!(s.summarize("transcript").unwrap(), Map::new());
        let sent = sent.lock().clone();
        assert_eq!(sent.len(), 1);
        let (path, request) = &sent[0];
        assert!(path.ends_with("/responses"), "{path}");
        assert_eq!(request["model"], model);
        assert_eq!(request["reasoning"], json!({"effort": "low"}));
        assert_eq!(request["text"]["format"]["schema"], *MEET_SCHEMA);
        assert_eq!(request["instructions"], SUMMARY_PROMPT);
        assert_eq!(request["max_output_tokens"], 2500);
        // metered as the meet notes, under the bare model name
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let rows = eve_spend::Ledger::at(d.path()).day_rows(&today);
        assert!(rows.iter().any(|r| r.get("f") == Some(&json!(eve_spend::MEET))), "{rows:?}");
    }
    assert!(summarizer(Some(&HashMap::new()), Default::default()).is_none());
}

#[test]
fn the_summary_is_the_last_of_a_long_transcript_and_a_bad_reply_fails() {
    struct Sdk(Mutex<Vec<Value>>, &'static str);
    impl eve_notes::NotesSdk for Sdk {
        fn transcriptions_create(&self, _: Map<String, Value>, _: eve_openrouter::Part) -> anyhow::Result<Value> {
            Ok(json!({}))
        }
        fn responses_create(&self, body: Value) -> anyhow::Result<Value> {
            self.0.lock().push(body);
            Ok(json!({"output": [{"type": "message", "content": [{"type": "output_text", "text": self.1}]}]}))
        }
    }
    let d = tempfile::tempdir().unwrap();
    let sdk = Arc::new(Sdk(Mutex::new(Vec::new()), "not json"));
    let s = Summarizer { model: "gpt-6-luna".into(), sdk: sdk.clone(), ledger: eve_spend::Ledger::at(d.path()) };
    let long = format!("{}{}", "é".repeat(10), "x".repeat(MAX_TRANSCRIPT_CHARS));
    let err = s.summarize(&long).unwrap_err();
    assert_eq!(py::error_name(&err), "JSONDecodeError");
    let input = sdk.0.lock()[0]["input"].as_str().unwrap().to_string();
    assert_eq!(input.chars().count(), MAX_TRANSCRIPT_CHARS);
    assert!(!input.contains('é'));
    let blank = Summarizer { model: "m".into(), sdk: Arc::new(Sdk(Mutex::new(Vec::new()), "  ")), ledger: eve_spend::Ledger::at(d.path()) };
    assert_eq!(blank.summarize("t").unwrap(), Map::new());
}

// ---- test_steel_local.py ----------------------------------------------------------------------------

#[test]
fn test_meet_keeps_eves_own_chrome_when_steel_drives_the_rest() {
    let d = tempfile::tempdir().unwrap();
    let env: HashMap<String, String> = HashMap::from([("EVE_MEET".into(), "1".into()), ("EVE_BROWSER_STEEL".into(), "1".into())]);
    let meeter = make_meeter(Some(configured(Some(&env))), d.path(), None, None, Some(&env)).unwrap();
    let cfg = meeter.lane_config.clone().unwrap();
    assert!(cfg.own && !cfg.steel && cfg.attach); // Meet needs audio: never Steel
    // the profile named for Meet wins over the lane's
    let named = MeetConfig { profile: "eve@example.com".into(), ..Default::default() };
    assert_eq!(meet_lane_config(&named, cfg.clone()).chrome_profile, "eve@example.com");
    assert_eq!(meet_lane_config(&MeetConfig::default(), cfg.clone()).chrome_profile, cfg.chrome_profile);
    // off without attach, and off by EVE_MEET
    let plain: HashMap<String, String> = HashMap::new();
    assert!(make_meeter(Some(MeetConfig::default()), d.path(), None, None, Some(&plain)).is_none());
    let off: HashMap<String, String> = HashMap::from([("EVE_MEET".into(), "0".into()), ("EVE_BROWSER_ATTACH".into(), "1".into())]);
    assert!(make_meeter(None, d.path(), None, None, Some(&off)).is_none());
}

// ---- test_apps_mcp.py ------------------------------------------------------------------------------

#[test]
fn test_an_event_is_compacted_for_the_model_and_keeps_what_meet_reads() {
    // the raw event and google_rest.compact_event's result for it (the compaction itself is apps_mcp's)
    let raw = json!({"kind": "calendar#event", "id": "e1", "status": "confirmed", "summary": "Standup",
                     "description": format!("Agenda. {} join https://meet.google.com/abc-defg-hij now", "y".repeat(900)),
                     "location": "Room 4, 1 Main St",
                     "start": {"dateTime": "2026-10-05T09:00:00-07:00"}, "end": {"dateTime": "2026-10-05T09:15:00-07:00"},
                     "hangoutLink": "https://meet.google.com/abc-defg-hij",
                     "conferenceData": {"entryPoints": [{"entryPointType": "video", "uri": "https://meet.google.com/abc-defg-hij"}]}});
    let out = json!({"id": "e1", "status": "confirmed", "summary": "Standup", "start": raw["start"], "end": raw["end"],
                     "location": "Room 4, 1 Main St",
                     "description": format!("Agenda. {}… https://meet.google.com/abc-defg-hij", "y".repeat(500)),
                     "hangoutLink": raw["hangoutLink"], "conferenceData": raw["conferenceData"]});
    assert_eq!(events_in(&json!({"events": [out]}), 0), vec![out.clone()]);
    assert_eq!(event_link(&out), event_link(&raw));
    assert_eq!(event_link(&out).as_deref(), Some("https://meet.google.com/abc-defg-hij?hl=en"));
    let only_desc = json!({"summary": "Sync", "start": {"dateTime": "x"},
                           "description": format!("{}… https://meet.google.com/abc-defg-hij", "z".repeat(500))});
    assert_eq!(event_link(&only_desc).as_deref(), Some("https://meet.google.com/abc-defg-hij?hl=en"));
}

#[test]
fn the_calendar_lister_asks_for_the_day_and_turns_a_refusal_into_an_error() {
    struct Apps(Mutex<Vec<(String, Value)>>, Value);
    impl CalendarApps for Apps {
        fn execute(&self, tool: &str, args: Value) -> anyhow::Result<Value> {
            self.0.lock().push((tool.into(), args));
            Ok(self.1.clone())
        }
    }
    let off = FixedOffset::west_opt(7 * 3600).unwrap();
    let (start, end) = (off.with_ymd_and_hms(2026, 9, 26, 0, 0, 0).unwrap(), off.with_ymd_and_hms(2026, 9, 27, 0, 0, 0).unwrap());
    let apps = Arc::new(Apps(Mutex::new(Vec::new()), json!({"ok": true, "items": []})));
    let list = calendar_lister(apps.clone());
    assert_eq!(list(start, end).unwrap(), json!({"ok": true, "items": []}));
    assert_eq!(
        apps.0.lock()[0],
        ("calendar__list_events".to_string(),
         json!({"startTime": "2026-09-26T00:00:00-07:00", "endTime": "2026-09-27T00:00:00-07:00", "orderBy": "startTime", "pageSize": 50}))
    );
    let refused = calendar_lister(Arc::new(Apps(Mutex::new(Vec::new()), json!({"ok": false, "reason": "not connected"}))));
    let e = refused(start, end).unwrap_err();
    assert_eq!((py::error_name(&e), py::error_text(&e)), ("RuntimeError".to_string(), "not connected".to_string()));
    let bare = calendar_lister(Arc::new(Apps(Mutex::new(Vec::new()), json!({"ok": false}))));
    assert_eq!(py::error_text(&bare(start, end).unwrap_err()), "the calendar call failed");
}
