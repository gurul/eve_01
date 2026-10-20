//! A differential check: `tests/fixtures/python_reference.json` holds what `notes.py` itself returned (2026-10-20)
//! for junk and edge inputs to `clean_summary`, `render`, `note_sections`, `merge_partials`, `_segments`, `_at`,
//! `_items`, `stitch`, `wants_stop`, `_slug`, `stamp` and `configured`; the port must return the same.

use crate::*;
use serde_json::{json, Map, Value};

#[test]
fn the_pure_functions_match_pythons_outputs() {
    let raw = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/python_reference.json")).unwrap();
    let d: Value = serde_json::from_str(&raw).unwrap();
    let lines = vec![Line::new(3.0, "Good morning. I think we ship Friday.", "Speaker 1"), Line::new(9.5, "Friday works, 12 servos!", ""),
        Line::new(65.0, "Ölçü is fine.  ok", "Speaker 2")];
    let mut bad = 0;
    for c in d["clean"].as_array().unwrap() {
        let got = Value::Object(clean_summary(&c["raw"], &lines));
        if got != c["want"] || got.to_string() != c["want"].to_string() { bad += 1; eprintln!("CLEAN {}\n got {}\nwant {}", c["raw"], got, c["want"]); }
    }
    for c in d["render"].as_array().unwrap() {
        let kw = c["kw"].as_object().unwrap();
        let opts = RenderOpts {
            keep_transcript: kw.get("keep_transcript").and_then(Value::as_bool).unwrap_or(true),
            source: kw.get("source").and_then(Value::as_str).unwrap_or("eve-notes").into(),
            pending: kw.get("pending").and_then(Value::as_bool).unwrap_or(false),
            speakers: kw.get("speakers").map(|v| v.as_array().unwrap().iter().map(|s| s.as_str().unwrap().to_string()).collect()),
            live: kw.get("live").and_then(Value::as_str).unwrap_or("").into(),
            diarized: kw.get("diarized").and_then(Value::as_str).unwrap_or("no").into(),
        };
        let st = NotesState { started_wall: Some(chrono::NaiveDate::from_ymd_opt(2026,1,2).unwrap().and_hms_opt(3,4,0).unwrap()), words: c["words"].as_u64().unwrap() as usize, ..Default::default() };
        let got = render(c["summary"].as_object().unwrap(), &"one two three ".repeat(50), &st, &opts);
        if got != c["want"].as_str().unwrap() { bad += 1; eprintln!("RENDER {} {}\n got {:?}\nwant {:?}", c["summary"], c["kw"], got, c["want"].as_str().unwrap()); }
    }
    for c in d["sections"].as_array().unwrap() {
        let sp: Option<Vec<String>> = c["speakers"].as_array().map(|a| a.iter().map(|s| s.as_str().unwrap().to_string()).collect());
        let got: Vec<Value> = note_sections(c["summary"].as_object().unwrap(), sp.as_deref()).into_iter().map(|(h, rows)| json!([h, rows.into_iter().map(|r| match r { Row::Text(t) => json!(t), Row::Topic(h, p) => json!([h, p]) }).collect::<Vec<_>>()])).collect();
        if Value::Array(got.clone()) != c["want"] { bad += 1; eprintln!("SECTIONS {}\n got {:?}\nwant {}", c["summary"], got, c["want"]); }
    }
    for c in d["merge"].as_array().unwrap() {
        let parts: Vec<Map<String, Value>> = c["parts"].as_array().unwrap().iter().map(|p| p.as_object().unwrap().clone()).collect();
        let got = Value::Object(merge_partials(&parts));
        if got.to_string() != c["want"].to_string() { bad += 1; eprintln!("MERGE\n got {}\nwant {}", got, c["want"]); }
    }
    for c in d["segments"].as_array().unwrap() {
        let got: Vec<Value> = segments(&c["raw"]).into_iter().map(|s| json!({"speaker": s.speaker, "start": s.start, "end": s.end, "text": s.text})).collect();
        if Value::Array(got.clone()) != c["want"] { bad += 1; eprintln!("SEGS\n got {:?}\nwant {}", got, c["want"]); }
    }
    for c in d["at"].as_array().unwrap() {
        let v = if c["v"].is_null() { None } else { Some(&c["v"]) };
        let got = at(v);
        if json!(got) != c["want"] { bad += 1; eprintln!("AT {} got {:?} want {}", c["v"], got, c["want"]); }
    }
    for c in d["items"].as_array().unwrap() {
        let got = items(Some(&c["v"]), None);
        if json!(got) != c["want"] { bad += 1; eprintln!("ITEMS got {:?} want {}", got, c["want"]); }
    }
    for c in d["stitch"].as_array().unwrap() {
        let got = stitch(c["a"].as_str().unwrap(), c["b"].as_str().unwrap());
        if json!(got) != c["want"] { bad += 1; eprintln!("STITCH {} got {:?}", c, got); }
    }
    for c in d["stop"].as_array().unwrap() {
        let got = wants_stop(c["s"].as_str().unwrap());
        if json!(got) != c["want"] { bad += 1; eprintln!("STOP {} got {:?}", c, got); }
    }
    for c in d["slug"].as_array().unwrap() {
        let got = slug(c["s"].as_str().unwrap());
        if json!(got) != c["want"] { bad += 1; eprintln!("SLUG {} got {:?}", c, got); }
    }
    for c in d["stamp"].as_array().unwrap() {
        let got = stamp(c["s"].as_f64().unwrap());
        if json!(got) != c["want"] { bad += 1; eprintln!("STAMP {} got {:?}", c, got); }
    }
    for c in d["conf"].as_array().unwrap() {
        let v = c["v"].as_str().unwrap();
        let env: std::collections::HashMap<String, String> = [("EVE_NOTES_SEGMENT_SECS", v.to_string()), ("EVE_NOTES_MAX_MINUTES", v.to_string()), ("EVE_NOTES_OWNER_NAME", v.repeat(30))].into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        let got = configured(Some(&env));
        if json!(got.segment_secs) != c["seg"] || json!(got.max_minutes) != c["max"] || json!(got.owner_name) != c["owner"] { bad += 1; eprintln!("CONF {} got {} {} {:?}", c, got.segment_secs, got.max_minutes, got.owner_name); }
    }
    assert_eq!(bad, 0);
}
