//! Random and edge inputs run through Python's nyt.py (tests/fixtures/differential.json, written by
//! tests/fixtures/gen_differential.py) and through this crate: every output must be the same.

use chrono::{TimeZone, Utc};
use eve_nyt::{self as nyt, ListName};
use serde_json::{json, Value};

fn cases() -> Value {
    serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/differential.json"))).unwrap()
}

fn rows<'a>(g: &'a Value, name: &str) -> &'a Vec<Value> {
    g[name].as_array().unwrap_or_else(|| panic!("no {name}"))
}

fn now() -> Option<chrono::DateTime<Utc>> {
    Some(Utc.with_ymd_and_hms(2026, 10, 16, 12, 0, 0).unwrap())
}

fn opt(v: &Value) -> Option<&Value> {
    if v.is_null() {
        None
    } else {
        Some(v)
    }
}

fn result<T: Into<Value>>(r: Result<T, nyt::BadArgs>) -> Value {
    match r {
        Ok(v) => json!({"ok": v.into()}),
        Err(e) => json!({"err": e.0}),
    }
}

#[test]
fn clean_text_matches_python() {
    for c in rows(&cases(), "clean_text") {
        let limit = c[1].as_u64().unwrap() as usize;
        assert_eq!(nyt::clean_text(opt(&c[0]), limit), c[2].as_str().unwrap(), "{:?} at {limit}", c[0]);
    }
}

#[test]
fn int_matches_python_where_python_answers() {
    for c in rows(&cases(), "int") {
        if c[1].get("raises").is_some() {
            continue; // Python's int() raises there (a crash of the call), see PARITY.md
        }
        assert_eq!(nyt::to_int(opt(&c[0])).map(Value::from).unwrap_or(Value::Null), c[1]["ok"], "{:?}", c[0]);
    }
}

#[test]
fn links_days_keys_and_dates_match_python() {
    let g = cases();
    for c in rows(&g, "https") {
        assert_eq!(nyt::https_url(opt(&c[0])).map(Value::from).unwrap_or(Value::Null), c[1], "{:?}", c[0]);
    }
    for c in rows(&g, "day") {
        assert_eq!(nyt::day(opt(&c[0])).map(Value::from).unwrap_or(Value::Null), c[1], "{:?}", c[0]);
    }
    for c in rows(&g, "key") {
        assert_eq!(nyt::key(&eve_nyt::py::str_or_empty(opt(&c[0]))), c[1].as_str().unwrap(), "{:?}", c[0]);
    }
    for c in rows(&g, "parse_date") {
        let got = nyt::parse_date(opt(&c[0]), "d").map(|d| d.map(|d| d.to_string()).map(Value::from).unwrap_or(Value::Null));
        assert_eq!(result(got), c[1], "{:?}", c[0]);
    }
    for c in rows(&g, "books_date") {
        let got = nyt::books_date(opt(&c[0]), now()).map(|d| d.map(|d| d.to_string()).map(Value::from).unwrap_or(Value::Null));
        assert_eq!(result(got), c[1], "{:?}", c[0]);
    }
}

#[test]
fn the_arguments_read_like_python() {
    let g = cases();
    for c in rows(&g, "fq") {
        assert_eq!(nyt::build_fq(opt(&c[0]), opt(&c[1])), c[2].as_str().unwrap(), "{c:?}");
    }
    for c in rows(&g, "search_params") {
        let got = nyt::search_params(&c[0], now()).map(Value::Object);
        let got = result(got);
        assert_eq!(got, c[1], "{:?}", c[0]);
        // the order of the query string too
        if let (Some(a), Some(b)) = (got["ok"].as_object(), c[1]["ok"].as_object()) {
            assert_eq!(a.keys().collect::<Vec<_>>(), b.keys().collect::<Vec<_>>());
        }
    }
    for c in rows(&g, "archive_month") {
        let got = nyt::archive_month(&c[0], now()).map(|(y, m)| json!([y, m]));
        assert_eq!(result(got), c[1], "{:?}", c[0]);
    }
    for c in rows(&g, "popular_path") {
        assert_eq!(result(nyt::popular_path(&c[0])), c[1], "{:?}", c[0]);
    }
    for c in rows(&g, "resolve_section") {
        let got = nyt::resolve_section(opt(&c[0])).map(|(t, r)| json!([t, r]));
        assert_eq!(result(got), c[1], "{:?}", c[0]);
    }
}

#[test]
fn the_list_names_and_matches_are_pythons() {
    let g = cases();
    let names = nyt::list_names(Some(&g["list_names_in"]));
    let as_json: Vec<Value> = names.iter().map(|n: &ListName| json!({"slug": n.slug, "name": n.name})).collect();
    assert_eq!(Value::Array(as_json), g["list_names"]);
    for c in rows(&g, "match_list") {
        let wanted = nyt::clean_text(opt(&c[0]), nyt::NAME_CHARS);
        let got = nyt::match_list(&wanted, &names).map(|n| Value::from(n.slug.clone())).unwrap_or(Value::Null);
        assert_eq!(got, c[1], "{:?}", c[0]);
    }
}

/// Equal values and the same key order, all the way down (the model reads the result as text).
fn same(a: &Value, b: &Value) -> bool {
    a == b && serde_json::to_string(a).unwrap() == serde_json::to_string(b).unwrap()
}

#[test]
fn the_rows_are_pythons() {
    let g = cases();
    for c in rows(&g, "article") {
        let got = nyt::article(&c[0]).unwrap_or(Value::Null);
        assert!(same(&got, &c[1]), "{:?}\n rust {got}\n python {}", c[0], c[1]);
    }
    for c in rows(&g, "archive_row") {
        let got = nyt::archive_row(&c[0]).unwrap_or(Value::Null);
        assert!(same(&got, &c[1]), "{:?}\n rust {got}\n python {}", c[0], c[1]);
    }
    for c in rows(&g, "book") {
        let got = nyt::book(&c[0]);
        assert!(same(&got, &c[1]), "{:?}\n rust {got}\n python {}", c[0], c[1]);
    }
    for c in rows(&g, "popular_row") {
        let got = nyt::popular_row(7, &c[0]).unwrap_or(Value::Null);
        assert!(same(&got, &c[1]), "{:?}\n rust {got}\n python {}", c[0], c[1]);
    }
    for c in rows(&g, "top_row") {
        let got = nyt::top_row(&c[0]).unwrap_or(Value::Null);
        assert!(same(&got, &c[1]), "{:?}\n rust {got}\n python {}", c[0], c[1]);
    }
}

#[test]
fn the_feeds_parse_like_python() {
    for c in rows(&cases(), "parse_rss") {
        let feed = c[0].as_str().unwrap();
        let got = match nyt::parse_rss(feed.as_bytes()) {
            Ok(rows) => json!({"ok": rows}),
            Err(e) => json!({"err": e.0}),
        };
        assert!(same(&got, &c[1]), "{feed}\n rust {got}\n python {}", c[1]);
    }
}

fn unhex(h: &str) -> Vec<u8> {
    (0..h.len()).step_by(2).map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap()).collect()
}

#[test]
fn feeds_in_other_encodings_parse_like_python() {
    for c in rows(&cases(), "parse_rss_bytes") {
        let feed = unhex(c[0].as_str().unwrap());
        let got = match nyt::parse_rss(&feed) {
            Ok(rows) => json!({"ok": rows}),
            Err(e) => json!({"err": e.0}),
        };
        assert!(same(&got, &c[1]), "{}\n rust {got}\n python {}", String::from_utf8_lossy(&feed), c[1]);
    }
}

// ---- whole calls ------------------------------------------------------------------------------------------------

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use nyt::{HttpRequest, HttpResponse, Opts, Spacer, Transport, TransportError};

const CANARY: &str = "canary-nyt-key-7f3a9c";

struct Replay {
    replies: Mutex<Vec<Value>>,
    urls: Mutex<Vec<String>>,
}

#[async_trait]
impl Transport for Replay {
    async fn get(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        self.urls.lock().unwrap().push(request.url.replace(CANARY, "KEY"));
        let reply = {
            let mut r = self.replies.lock().unwrap();
            if r.len() > 1 { r.remove(0) } else { r[0].clone() }
        };
        match reply.as_str() {
            Some("timeout") => return Err(TransportError::Timeout),
            Some(_) => return Err(TransportError::Failed("ConnectError".into())),
            None => {}
        }
        let status = reply[0].as_u64().unwrap() as u16;
        let body = unhex(reply[1].as_str().unwrap());
        let headers = reply[2].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string())).collect();
        Ok(HttpResponse { status, headers, body: futures::stream::iter(vec![Ok(Bytes::from(body))]).boxed() })
    }
}

fn free() -> Arc<Spacer> {
    let t = Arc::new(Mutex::new(0.0f64));
    Arc::new(Spacer::with_clock(nyt::KEY_SPACING_SECS, nyt::KEY_PER_DAY, Box::new(move || {
        let mut v = t.lock().unwrap();
        *v += 60.0;
        *v
    })))
}

/// host + path and the decoded query pairs in order (the two daemons percent-encode `*` and `~` differently).
fn wire(u: &str) -> (String, Vec<(String, String)>) {
    let u = url::Url::parse(u).unwrap();
    (format!("{}{}", u.host_str().unwrap(), u.path()), u.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect())
}

#[tokio::test]
async fn whole_calls_answer_and_ask_like_python() {
    for c in rows(&cases(), "calls") {
        let dir = tempfile::tempdir().unwrap();
        let replay = Arc::new(Replay { replies: Mutex::new(c["replies"].as_array().unwrap().clone()), urls: Mutex::new(Vec::new()) });
        let opts = Opts {
            transport: replay.clone(),
            sleep: Arc::new(|_| Box::pin(async {})),
            limiter: free(),
            rss_limiter: free(),
            now: now(),
            cache_dir: Some(dir.path().to_path_buf()),
        };
        let mut env = nyt::Environ::from([(nyt::SWITCH.to_string(), "1".to_string())]);
        if c["key"] == json!(true) {
            env.insert(nyt::KEY_ENV.into(), CANARY.into());
        }
        let name = c["name"].as_str().unwrap();
        let got = nyt::handle(name, &c["args"], Some(&env), &opts).await;
        assert!(same(&got, &c["result"]), "{name} {}\n rust {got}\n python {}", c["args"], c["result"]);
        let urls: Vec<_> = replay.urls.lock().unwrap().iter().map(|u| wire(u)).collect();
        let want: Vec<_> = c["urls"].as_array().unwrap().iter().map(|u| wire(u.as_str().unwrap())).collect();
        assert_eq!(urls, want, "{name} {}", c["args"]);
    }
}
