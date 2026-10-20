//! Optional practice references through TinyFish search; learner work and images are never sent.
//!
//! Only the topic and level leave the Mac. One TinyFish search and one fetch of its top three results
//! (`web_reader::search_and_read`, through the watcher's guarded `http_request`) give the pages as markdown; the
//! tutor uses them as inspiration for an original, adapted problem. Exa did this until 2026-09-26, with its
//! content moderation on; TinyFish's search has no such option (docs.tinyfish.ai, 2026-09-29), so the tutor
//! prompt, which adapts rather than copies, is the filter now.

use eve_web::Ctx;
use serde_json::{json, Value};

use crate::py::{py_strip, take_chars};

pub const RESULTS: usize = 3;
pub const TEXT_CHARS: usize = 3000;

/// One practice reference handed to the tutor: `{"title", "url", "text"}`.
pub type Source = serde_json::Map<String, Value>;

/// What `urllib.parse.urlsplit` says about a URL's authority: (scheme, hostname, username, password).
/// The hostname is lowercased and IPv6 brackets are dropped, as Python's `hostname` property does.
pub(crate) fn split_authority(url: &str) -> (String, Option<String>, Option<String>, Option<String>) {
    let cleaned: String =
        url.trim_start_matches(|c: char| c <= ' ').chars().filter(|c| !matches!(c, '\t' | '\r' | '\n')).collect();
    let mut scheme = String::new();
    let mut rest = cleaned.as_str();
    if let Some(i) = cleaned.find(':') {
        let s = &cleaned[..i];
        let mut chars = s.chars();
        if chars.next().is_some_and(|c| c.is_ascii_alphabetic())
            && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        {
            scheme = s.to_ascii_lowercase();
            rest = &cleaned[i + 1..];
        }
    }
    let Some(after) = rest.strip_prefix("//") else { return (scheme, None, None, None) };
    let end = after.find(['/', '?', '#']).unwrap_or(after.len());
    let netloc = &after[..end];
    let (userinfo, hostinfo) = match netloc.rfind('@') {
        Some(i) => (Some(&netloc[..i]), &netloc[i + 1..]),
        None => (None, netloc),
    };
    let (username, password) = match userinfo {
        Some(info) => match info.split_once(':') {
            Some((u, p)) => (Some(u.to_string()), Some(p.to_string())),
            None => (Some(info.to_string()), None),
        },
        None => (None, None),
    };
    let host = if let Some(inner) = hostinfo.strip_prefix('[') {
        inner.split(']').next().unwrap_or("")
    } else {
        hostinfo.split(':').next().unwrap_or("")
    };
    let hostname = if host.is_empty() { None } else { Some(host.to_lowercase()) };
    (scheme, hostname, username, password)
}

/// Practice references for a topic and level, and the note the learner sees about them.
pub fn search_problems(ctx: &Ctx, topic: &str, level: &str) -> (Vec<Source>, String) {
    if eve_web::tinyfish::api_key(ctx).is_empty() {
        return (
            Vec::new(),
            "TinyFish search is off. Add TINYFISH_API_KEY to ~/.config/eve-bridge/env and restart \
to enable practice references."
                .to_string(),
        );
    }
    let query =
        format!("Practice problems and exercises: {}. Level: {}", take_chars(topic, 200), take_chars(level, 100));
    let pages = match eve_web::web_reader::search_and_read(ctx, &query, RESULTS) {
        Ok(pages) => pages,
        Err(exc) => {
            let why = if exc.status != 0 { format!(" (HTTP {})", exc.status) } else { String::new() };
            return (Vec::new(), format!("TinyFish search failed{why}. Eve created a problem without search references."));
        }
    };
    ctx.ledger.record(
        "tinyfish",
        "search+fetch",
        eve_spend::LESSONS,
        Some(0.0),
        eve_spend::RecordOpts::new().note(&format!("{} pages, free", pages.len())),
    );
    let mut sources = Vec::new();
    for page in &pages {
        let (url, content) = (&page.url, &page.markdown);
        if py_strip(content).is_empty() {
            continue;
        }
        let (scheme, hostname, username, password) = split_authority(url);
        let Some(hostname) = hostname.filter(|_| scheme == "https") else { continue };
        if username.is_some_and(|u| !u.is_empty()) || password.is_some_and(|p| !p.is_empty()) {
            continue;
        }
        let title = if page.title.is_empty() { hostname } else { page.title.clone() };
        let source = json!({"title": take_chars(&title, 200), "url": take_chars(url, 2000),
                            "text": take_chars(content, TEXT_CHARS)});
        if let Value::Object(m) = source {
            sources.push(m);
        }
    }
    let note = if sources.is_empty() {
        "TinyFish found no usable references. Eve created a problem without search references."
    } else {
        "Practice references found with TinyFish; Eve creates an adapted problem."
    };
    (sources, note.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use eve_web::{FetchError, HttpOptions};
    use parking_lot::Mutex;
    use std::collections::HashMap;
    use std::sync::Arc;

    /// A sent request: url, JSON body, headers.
    pub(crate) type Sent = Arc<Mutex<Vec<(String, Option<Value>, Vec<(String, String)>)>>>;

    /// A fake `watch.http_request` answering TinyFish's search (GET) and fetch (POST) with `pages`
    /// ({"url", "title", "markdown"}); records what was sent.
    pub(crate) fn tinyfish_api(pages: Vec<(String, String, String)>) -> (Arc<dyn eve_web::Http>, Sent) {
        let sent: Sent = Arc::new(Mutex::new(Vec::new()));
        let log = sent.clone();
        let fake = move |url: &str, opts: &HttpOptions| -> Result<(u16, String), FetchError> {
            let body = opts.data.as_ref().map(|d| serde_json::from_slice::<Value>(d).unwrap());
            log.lock().push((url.to_string(), body.clone(), opts.headers.clone()));
            if body.is_none() {
                let results: Vec<Value> =
                    pages.iter().map(|(u, t, _)| json!({"url": u, "title": t, "snippet": ""})).collect();
                return Ok((200, json!({"query": "q", "results": results}).to_string()));
            }
            let results: Vec<Value> = pages.iter().map(|(u, t, m)| json!({"url": u, "title": t, "text": m})).collect();
            Ok((200, json!({"results": results, "errors": []}).to_string()))
        };
        (Arc::new(fake), sent)
    }

    pub(crate) fn ctx(env: &[(&str, &str)], http: Arc<dyn eve_web::Http>, ledger: &std::path::Path) -> Ctx {
        let env: HashMap<String, String> = env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        Ctx::global().clone().with_env(env).with_http(http).with_ledger(eve_spend::Ledger::at(ledger))
    }

    fn query_of(url: &str) -> HashMap<String, String> {
        let (_, q) = url.split_once('?').unwrap();
        q.split('&')
            .map(|kv| {
                let (k, v) = kv.split_once('=').unwrap();
                (k.to_string(), urlencoding_decode(&v.replace('+', " ")))
            })
            .collect()
    }

    fn urlencoding_decode(s: &str) -> String {
        let bytes = s.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
                i += 3;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn test_missing_key() {
        let tmp = tempfile::tempdir().unwrap();
        let fail = |_: &str, _: &HttpOptions| -> Result<(u16, String), FetchError> { panic!("no key, no request") };
        let c = ctx(&[], Arc::new(fail), tmp.path());
        let (sources, note) = search_problems(&c, "Counting", "Kindergarten");
        assert!(sources.is_empty() && note.contains("TINYFISH_API_KEY"));
    }

    #[test]
    fn test_search_failure() {
        // Every failure reaches search_problems as a FetchError (Python's TimeoutError and ValueError from the
        // request come out of watch.http_request as one): with a status and without.
        for failure in [FetchError::with_status("the site answered HTTP 402", 402), FetchError::new("the site took too long to answer"), FetchError::new("the site sent an unreadable answer")] {
            let tmp = tempfile::tempdir().unwrap();
            let f = failure.clone();
            let boom = move |_: &str, _: &HttpOptions| -> Result<(u16, String), FetchError> { Err(f.clone()) };
            let c = ctx(&[("TINYFISH_API_KEY", "tf-secret")], Arc::new(boom), tmp.path());
            let (sources, note) = search_problems(&c, "Algebra", "Grade 7");
            assert!(sources.is_empty() && note.contains("without search references"), "{note}");
            assert!(!note.contains("tf-secret"));
            assert_eq!(note.contains("(HTTP 402)"), failure.status == 402);
        }
    }

    #[test]
    fn test_search_is_subject_neutral() {
        let tmp = tempfile::tempdir().unwrap();
        let pages = "abcde".chars().map(|c| (format!("https://{c}.example"), "T".to_string(), "text".to_string())).collect();
        let (http, sent) = tinyfish_api(pages);
        let c = ctx(&[("TINYFISH_API_KEY", "tf-secret")], http, tmp.path());
        let (sources, note) = search_problems(&c, "Rust ownership and borrowing", "senior backend engineer, new to Rust");
        let sent = sent.lock();
        let query = query_of(&sent[0].0);
        assert!(sources.len() == 3 && note.contains("found with TinyFish"));
        assert_eq!(query.keys().collect::<Vec<_>>(), vec!["query"]);
        assert!(query["query"].contains("Rust ownership") && query["query"].contains("senior backend engineer"));
        assert!(!query["query"].contains("Math"));
        let body = sent[1].1.as_ref().unwrap();
        assert_eq!(body["urls"], json!(["https://a.example", "https://b.example", "https://c.example"]));
        assert_eq!(body["format"], json!("markdown"));
    }

    #[test]
    fn only_https_references_without_credentials_are_kept_and_the_search_is_metered() {
        let tmp = tempfile::tempdir().unwrap();
        let pages = vec![
            ("https://openstax.org/books/algebra".to_string(), "Algebra".to_string(), "Equation exercises".to_string()),
            ("https://user:pw@secret.example/x".to_string(), "Algebra".to_string(), "Equation exercises".to_string()),
            ("https://EMPTY.example/x".to_string(), "".to_string(), "x".repeat(5000)),
            // past RESULTS: never read
            ("https://fourth.example/x".to_string(), "Algebra".to_string(), "Equation exercises".to_string()),
        ];
        let (http, _) = tinyfish_api(pages);
        let c = ctx(&[("TINYFISH_API_KEY", "tf-secret")], http, tmp.path());
        let (sources, _) = search_problems(&c, "Algebra", "Grade 7");
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0]["url"], json!("https://openstax.org/books/algebra"));
        assert_eq!(sources[1]["title"], json!("empty.example"));
        assert_eq!(sources[1]["text"].as_str().unwrap().len(), TEXT_CHARS);
        let today = chrono_free_today(tmp.path());
        let line: Value = serde_json::from_str(today.lines().next().unwrap()).unwrap();
        assert_eq!((line["p"].as_str(), line["f"].as_str(), line["usd"].as_f64(), line["note"].as_str()),
                   (Some("tinyfish"), Some("lessons"), Some(0.0), Some("3 pages, free")));
    }

    /// The one spend ledger file written in `dir`.
    pub(crate) fn chrono_free_today(dir: &std::path::Path) -> String {
        let mut files: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).collect();
        files.sort();
        let file = files.iter().find(|p| p.extension().is_some_and(|e| e == "jsonl")).expect("a ledger file");
        std::fs::read_to_string(file).unwrap()
    }

    #[test]
    fn split_authority_reads_like_urlsplit() {
        assert_eq!(split_authority("HTTPS://Ex.ORG/a"), ("https".into(), Some("ex.org".into()), None, None));
        assert_eq!(split_authority("https://u@h:1/x").2, Some("u".into()));
        assert_eq!(split_authority("https://[::1]:8/x").1, Some("::1".into()));
        assert_eq!(split_authority("javascript:alert(1)"), ("javascript".into(), None, None, None));
    }
}
