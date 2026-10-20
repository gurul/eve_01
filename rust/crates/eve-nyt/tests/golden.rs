//! Outputs Python's nyt.py gave for the same inputs (tests/fixtures/golden.json, written by running the Python
//! module): text cleaning, integers, links, dates, keys, Lucene filters and the unknown-section reason.

use eve_nyt as nyt;
use serde_json::Value;

fn golden() -> Value {
    serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/golden.json"))).unwrap()
}

fn rows<'a>(g: &'a Value, name: &str) -> &'a Vec<Value> {
    g[name].as_array().unwrap()
}

#[test]
fn clean_text_matches_python() {
    let g = golden();
    let cases = rows(&g, "clean_text");
    let half = cases.len() / 2;
    for (i, case) in cases.iter().enumerate() {
        let limit = if i < half { 120 } else { 10 };
        assert_eq!(nyt::clean_text(Some(&case[0]), limit), case[1].as_str().unwrap(), "{:?} at {limit}", case[0]);
    }
    for case in rows(&g, "clean_text_values") {
        assert_eq!(nyt::clean_text(Some(&case[0]), 50), case[1].as_str().unwrap(), "{:?}", case[0]);
    }
}

#[test]
fn int_matches_python() {
    for case in rows(&golden(), "int") {
        assert_eq!(nyt::to_int(Some(&case[0])), case[1].as_i64(), "{:?}", case[0]);
    }
}

#[test]
fn links_dates_and_keys_match_python() {
    let g = golden();
    for case in rows(&g, "https") {
        assert_eq!(nyt::https_url(Some(&case[0])).as_deref(), case[1].as_str(), "{:?}", case[0]);
    }
    for case in rows(&g, "days") {
        assert_eq!(nyt::day(Some(&case[0])).as_deref(), case[1].as_str(), "{:?}", case[0]);
    }
    for case in rows(&g, "keys") {
        let text = eve_nyt::py::str_or_empty(Some(&case[0]));
        assert_eq!(nyt::key(&text), case[1].as_str().unwrap(), "{:?}", case[0]);
    }
}

#[test]
fn fq_matches_python() {
    for case in rows(&golden(), "fq") {
        let opt = |v: &Value| if v.is_null() { None } else { Some(v.clone()) };
        let (s, m) = (opt(&case[0]), opt(&case[1]));
        assert_eq!(nyt::build_fq(s.as_ref(), m.as_ref()), case[2].as_str().unwrap(), "{case:?}");
    }
}

#[test]
fn the_unknown_section_reason_matches_python() {
    let g = golden();
    let err = nyt::resolve_section(Some(&Value::String("gardening".into()))).unwrap_err();
    assert_eq!(err.0, g["resolve_error"].as_str().unwrap());
}
