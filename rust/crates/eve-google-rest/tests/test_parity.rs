//! The port against what the live Python answered (tests/fixtures/python_parity.json, written by
//! gen_parity.py on CPython 3.13 in Los Angeles time): every case must come out the same.

mod common;

use common::LA;
use eve_google_rest::{self as gr, pytime, Error};
use serde_json::{json, Value};

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/python_parity.json")).unwrap()
}

fn outcome<T>(r: Result<T, Error>, show: impl Fn(T) -> Value) -> Value {
    match r {
        Ok(v) => json!({"ok": show(v)}),
        Err(e) => json!({"err": format!("{}: {e}", e.type_name())}),
    }
}

#[test]
fn fromisoformat_matches_cpython() {
    let f = fixture();
    for (s, want) in f["fromisoformat"].as_object().unwrap() {
        assert_eq!(outcome(pytime::fromisoformat(s), |d| json!(d.isoformat())), *want, "{s:?}");
    }
    for (s, want) in f["date_fromisoformat"].as_object().unwrap() {
        let got = outcome(pytime::date_fromisoformat(s), |d| json!(d.format("%Y-%m-%d").to_string()));
        assert_eq!(got, *want, "{s:?}");
    }
}

#[test]
fn rfc3339_matches_python_in_los_angeles() {
    for (s, want) in fixture()["rfc3339"].as_object().unwrap() {
        assert_eq!(gr::rfc3339_in(&json!(s), LA), *want, "{s:?}");
    }
    assert_eq!(gr::rfc3339_in(&Value::Null, LA), "");
}

#[test]
fn local_zone_reads_the_macs_zone() {
    let name = gr::local_zone();
    assert!(name == "UTC" || name.parse::<chrono_tz::Tz>().is_ok(), "{name}");
}

#[test]
fn html_unescape_matches_python() {
    for (s, want) in fixture()["unescape"].as_object().unwrap() {
        assert_eq!(gr::html_unescape(s), *want, "{s:?}");
    }
}

#[test]
fn mail_text_matches_python() {
    for case in fixture()["mail_text"].as_array().unwrap() {
        assert_eq!(gr::mail_text(&case[0]), case[1], "{}", case[0]);
    }
}

#[test]
fn place_links_match_python() {
    for case in fixture()["place_links"].as_array().unwrap() {
        assert_eq!(Value::Object(gr::place_links(&case[0])), case[1], "{}", case[0]);
    }
}

#[test]
fn event_descriptions_match_python() {
    for case in fixture()["event_description"].as_array().unwrap() {
        let out = gr::compact_event(&json!({"description": case[0]}));
        assert_eq!(out["description"], case[1], "{}", case[0]);
    }
}

#[test]
fn addresses_match_python() {
    for case in fixture()["addresses"].as_array().unwrap() {
        assert_eq!(outcome(gr::addresses(&case[0]), |v| json!(v)), case[1], "{}", case[0]);
    }
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().unwrap().iter().map(|s| s.as_str().unwrap().to_string()).collect()
}

#[test]
fn build_mail_is_byte_for_byte_pythons() {
    use base64::Engine;
    let f = fixture();
    let cases = f["build_mail"].as_array().unwrap();
    assert!(cases.len() >= 45);
    for case in cases {
        let c = &case[0];
        let reply: Vec<(String, String)> = c[5]
            .as_object()
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string())).collect())
            .unwrap_or_default();
        let raw = gr::build_mail(&strings(&c[0]), &strings(&c[1]), &strings(&c[2]), c[3].as_str().unwrap(),
                                 c[4].as_str().unwrap(), &reply);
        if let Some(err) = case[1].get("err") {
            let e = raw.expect_err(&format!("{c}"));
            assert_eq!(format!("{}: {e}", e.type_name()), *err, "{c}");
            continue;
        }
        let raw = raw.unwrap();
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(raw).unwrap();
        let latin1: String = bytes.iter().map(|&b| b as char).collect();
        assert_eq!(latin1, case[1].as_str().unwrap(), "{c}");
    }
}

#[test]
fn free_slots_match_python_in_los_angeles() {
    for case in fixture()["free_slots"].as_array().unwrap() {
        let c = &case[0];
        let parse = |v: &Value| match pytime::fromisoformat(v.as_str().unwrap()).unwrap() {
            pytime::PyDateTime::Aware(d) => d,
            pytime::PyDateTime::Naive(_) => panic!("aware times only"),
        };
        let busy: Vec<_> = c[0].as_array().unwrap().iter().map(|b| (parse(&b[0]), parse(&b[1]))).collect();
        let got = gr::free_slots(&busy, parse(&c[1]), parse(&c[2]), c[3].as_i64().unwrap(), c[4].as_i64().unwrap(),
                                 c[5].as_i64().unwrap(), LA);
        assert_eq!(outcome(got, |v| json!(v)), case[1], "{c}");
    }
}

#[test]
fn tools_are_pythons_verbatim() {
    let want = &fixture()["tools"];
    let got: serde_json::Map<String, Value> =
        gr::TOOLS.iter().map(|(app, tools)| (app.to_string(), Value::Array(tools.clone()))).collect();
    assert_eq!(serde_json::to_string(&Value::Object(got)).unwrap(), serde_json::to_string(want).unwrap());
}
