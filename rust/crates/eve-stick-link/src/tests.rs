//! Ports of bridge/tests/test_stick_link.py (the parts that test stick_link.py) and the `latest_address` tests of
//! bridge/tests/test_link_pair_cli.py, plus parity checks against the Python module and a loopback HTTP server.

use super::*;
use std::io::{BufRead, BufReader, Read as _};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::sync::mpsc;
use std::thread;

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

// ---- the token ------------------------------------------------------------------------------------------

#[test]
fn test_no_token_until_pairing_and_nothing_is_accepted_then() {
    let dir = tmp();
    let t = LinkToken::new(dir.path().join("stick-link.json"));
    assert_eq!(t.current(), "");
    assert!(!t.check("") && !t.check("anything"));
}

#[test]
fn test_the_token_is_private_checked_exactly_and_a_new_one_cuts_off_the_old() {
    let dir = tmp();
    let t = LinkToken::new(dir.path().join("stick-link.json"));
    let first = t.ensure().unwrap();
    assert!(first.len() >= 40 && t.ensure().unwrap() == first); // ensure keeps it
    assert_eq!(fs::metadata(&t.path).unwrap().permissions().mode() & 0o7777, 0o600);
    assert!(t.check(&first)); // positive control
    assert!(!t.check(&first[..first.len() - 1]) && !t.check(&format!("{first}x")) && !t.check(""));
    let second = t.rotate().unwrap();
    assert!(second != first && t.check(&second) && !t.check(&first));
}

#[test]
fn test_a_damaged_token_file_counts_as_unpaired() {
    let dir = tmp();
    let path = dir.path().join("stick-link.json");
    for text in ["not json", r#"{"token": 5}"#, r#"{"token": "short"}"#, "[1]"] {
        fs::write(&path, text).unwrap();
        assert_eq!(LinkToken::new(&path).current(), "", "{text}");
    }
}

#[test]
fn test_the_pairing_link_keeps_the_token_out_of_what_the_server_sees() {
    let link = pair_url("https://a-b.trycloudflare.com/", "tok_en-123");
    let parts = url::Url::parse(&link).unwrap();
    assert!(parts.path() == PAIR_PATH && parts.query().unwrap_or("").is_empty()); // the request line carries neither
    assert_eq!(parts.fragment(), Some("t=tok_en-123"));
}

#[test]
fn test_pairing_without_a_tunnel_keeps_no_token() {
    // The stick_link half of the Python test: merely naming the file (as MiniApp does) writes nothing; only
    // ensure or rotate does.
    let dir = tmp();
    let t = LinkToken::new(dir.path().join("stick-link.json"));
    assert_eq!(t.current(), "");
    assert!(!t.check(""));
    assert!(!dir.path().join("stick-link.json").exists());
}

// ---- the rendezvous ---------------------------------------------------------------------------------------

#[test]
fn test_the_rendezvous_topic_is_the_phones_and_says_nothing_about_the_token() {
    let token = "a".repeat(40);
    // the same vector ios/EveLink/Tests/main.swift checks Rendezvous.topic against
    assert_eq!(rendezvous_topic(&token), "evelink-a7c4bc0e3e702a6abc8c7afe80e62b5d");
    assert!(!rendezvous_topic(&token).contains(&token));
    assert_ne!(rendezvous_topic(&"b".repeat(40)), rendezvous_topic(&token));
}

#[test]
fn test_publish_posts_only_the_address_to_the_tokens_topic() {
    let mut sent: Vec<(String, Vec<u8>)> = Vec::new();
    let ok = publish_address_with(
        "https://new-name.trycloudflare.com/",
        &"a".repeat(40),
        "https://ntfy.example",
        |url, body| {
            sent.push((url.to_owned(), body.to_vec()));
            Ok(200)
        },
    );
    assert!(ok);
    assert_eq!(
        sent,
        vec![(
            "https://ntfy.example/evelink-a7c4bc0e3e702a6abc8c7afe80e62b5d".to_owned(),
            b"https://new-name.trycloudflare.com".to_vec()
        )]
    );
}

#[test]
fn test_publish_failures_are_quiet_falses() {
    let a40 = "a".repeat(40);
    let boom = |_: &str, _: &[u8]| Err(TransportError::new("OSError"));
    assert!(!publish_address_with("https://x.trycloudflare.com", &a40, RENDEZVOUS_URL, boom));
    assert!(!publish_address_with("https://x.trycloudflare.com", &a40, RENDEZVOUS_URL, |_, _| Ok(500)));
    assert!(!publish_address_with("", &a40, RENDEZVOUS_URL, |_, _| Ok(200)));
    assert!(!publish_address_with("https://x.trycloudflare.com", "", RENDEZVOUS_URL, |_, _| Ok(200)));
    assert!(!publish_address_with("https://x.trycloudflare.com", &a40, "", |_, _| Ok(200)));
}

fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let pairs: Vec<(String, String)> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    move |name| pairs.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
}

#[test]
fn test_the_rendezvous_switch_and_server() {
    assert_eq!(rendezvous_base_from(env_of(&[])), "https://ntfy.sh");
    assert_eq!(rendezvous_base_from(env_of(&[("EVE_LINK_RENDEZVOUS", "0")])), "");
    assert_eq!(
        rendezvous_base_from(env_of(&[("EVE_LINK_RENDEZVOUS_URL", "https://ntfy.home/")])),
        "https://ntfy.home"
    );
}

#[test]
fn test_publish_never_raises() {
    // Rendezvous.lean finding 2: a reply that breaks http.client, or a base with no scheme, raised out of publish
    for name in ["BadStatusLine", "ValueError"] {
        let boom = |_: &str, _: &[u8]| Err(TransportError::new(name));
        assert!(!publish_address_with("https://x.trycloudflare.com", &"a".repeat(40), "ntfy.sh", boom));
    }
    // and the real post, with a base that has no scheme
    assert!(!publish_address("https://x.trycloudflare.com", &"a".repeat(40), "ntfy.sh"));
}

// ---- latest_address (bridge/tests/test_link_pair_cli.py) ------------------------------------------------------

const PAIR_TOKEN_LEN: usize = 43;

fn ndjson(messages: &[&str]) -> Vec<u8> {
    let mut rows = vec![serde_json::json!({"event": "open"})];
    rows.extend(messages.iter().map(|m| serde_json::json!({"event": "message", "message": m})));
    rows.iter().map(|r| r.to_string()).collect::<Vec<_>>().join("\n").into_bytes()
}

#[test]
fn test_the_newest_quick_tunnel_address_is_read_from_the_rendezvous() {
    let token = "t".repeat(PAIR_TOKEN_LEN);
    let mut seen: Vec<String> = Vec::new();
    let got = latest_address_with(&token, "https://ntfy.example", |url, timeout| {
        assert_eq!(timeout, Duration::from_secs(10));
        seen.push(url.to_owned());
        Ok(ndjson(&["https://old-one.trycloudflare.com", "https://new-one.trycloudflare.com/"]))
    });
    assert_eq!(got, "https://new-one.trycloudflare.com");
    assert_eq!(seen, vec![format!("https://ntfy.example/{}/json?poll=1&since=all", rendezvous_topic(&token))]);
    assert!(!seen[0].contains(&token)); // the topic, never the token
}

#[test]
fn test_anything_but_a_bare_quick_tunnel_origin_is_ignored() {
    let token = "t".repeat(PAIR_TOKEN_LEN);
    let got = latest_address_with(&token, "https://ntfy.example", |_, _| {
        Ok(ndjson(&[
            "https://new-one.trycloudflare.com",
            "https://evil.example.com",
            "https://x.trycloudflare.com/path",
            "not a url",
        ]))
    });
    assert_eq!(got, "https://new-one.trycloudflare.com");
}

#[test]
fn test_no_address_or_a_failed_read_is_empty() {
    let token = "t".repeat(PAIR_TOKEN_LEN);
    assert_eq!(latest_address_with(&token, "https://ntfy.example", |_, _| Ok(ndjson(&[]))), "");
    assert_eq!(
        latest_address_with(&token, "https://ntfy.example", |_, _| Err(TransportError::new("OSError"))),
        ""
    );
    assert_eq!(latest_address_with("", "https://ntfy.example", |_, _| Ok(ndjson(&[]))), "");
}

#[test]
fn latest_address_skips_lines_that_are_not_message_events() {
    let token = "t".repeat(PAIR_TOKEN_LEN);
    let body = concat!(
        "{\"event\":\"message\",\"message\":\"https://a.trycloudflare.com\"}\r\n",
        "garbage\n",
        "{\"event\":\"keepalive\",\"message\":\"https://b.trycloudflare.com\"}\n",
        "{\"event\":\"message\",\"message\":5}\n",
        "{\"event\":\"message\",\"message\":\"  https://c.trycloudflare.com// \"}\u{2028}",
        "{\"event\":\"message\",\"message\":\"https://UPPER.trycloudflare.com\"}\n",
        "{\"event\":\"message\",\"message\":\"https://.trycloudflare.com\"}\n",
    );
    let got = latest_address_with(&token, "https://ntfy.example/", |url, _| {
        assert!(url.starts_with("https://ntfy.example/evelink-"));
        Ok(body.as_bytes().to_vec())
    });
    assert_eq!(got, "https://c.trycloudflare.com");
}

// ---- parity details ---------------------------------------------------------------------------------------

fn python_source() -> String {
    fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/src/eve_bridge/stick_link.py")).unwrap()
}

#[test]
fn the_pairing_page_is_the_python_page_byte_for_byte() {
    let src = python_source();
    let start = src.find("PAIR_PAGE = \"\"\"").unwrap() + "PAIR_PAGE = \"\"\"".len();
    let end = start + src[start..].find("\"\"\"").unwrap();
    assert_eq!(PAIR_PAGE, &src[start..end]);
    assert!(PAIR_PAGE.contains("location.hash") && PAIR_PAGE.contains("evelink://pair?u="));
    assert!(PAIR_PAGE.contains("history.replaceState"));
    assert!(PAIR_HEADERS[0].1.contains("default-src 'none'"));
}

#[test]
fn the_token_file_is_written_as_python_writes_it() {
    let dir = tmp();
    let path = dir.path().join("nested/dir/stick-link.json");
    let t = LinkToken::new(&path);
    let token = t.ensure().unwrap();
    assert_eq!(token.len(), 43);
    assert!(token.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
    let text = fs::read_to_string(&path).unwrap();
    let prefix = format!("{{\"token\": \"{token}\", \"created\": ");
    assert!(text.starts_with(&prefix), "{text}");
    let created: i64 = text[prefix.len()..text.len() - 1].parse().unwrap();
    assert!(created > 1_700_000_000 && text.ends_with('}'));
    assert!(!dir.path().join("nested/dir/stick-link.tmp").exists()); // replaced into place
    // a token of 32 characters or more is read whatever wrote it; a longer file with other keys too
    fs::write(&path, format!("{{\"created\": 1, \"token\": \"{}\", \"x\": [1]}}", "é".repeat(32))).unwrap();
    assert_eq!(t.current(), "é".repeat(32));
    fs::write(&path, format!("{{\"token\": \"{}\"}}", "a".repeat(31))).unwrap();
    assert_eq!(t.current(), "");
}

#[test]
fn quoting_and_splitting_match_python() {
    assert_eq!(quote_all("aZ09_.-~ /#?&=+é%"), "aZ09_.-~%20%2F%23%3F%26%3D%2B%C3%A9%25");
    assert_eq!(
        py_splitlines("a\r\nb\rc\nd\u{0b}e\u{0c}f\u{1c}g\u{1d}h\u{1e}i\u{85}j\u{2028}k\u{2029}l\n"),
        vec!["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l"]
    );
    assert_eq!(py_splitlines("\n\nx"), vec!["", "", "x"]);
    assert_eq!(py_splitlines(""), Vec::<&str>::new());
    assert_eq!(py_strip("\u{1f} x \u{3000}"), "x");
    assert!(compare_digest(b"abc", b"abc") && !compare_digest(b"abc", b"abd") && !compare_digest(b"ab", b"abc"));
    assert_eq!(with_suffix_tmp(Path::new("/x/stick-link.json")), PathBuf::from("/x/stick-link.tmp"));
    assert_eq!(with_suffix_tmp(Path::new("/x/a.b.json")), PathBuf::from("/x/a.b.tmp"));
}

#[test]
fn the_rendezvous_switch_reads_like_python() {
    for off in ["0", "false", "No", " OFF ", "\toff\n"] {
        assert_eq!(rendezvous_base_from(env_of(&[("EVE_LINK_RENDEZVOUS", off)])), "", "{off:?}");
    }
    for on in ["", "1", "yes", "nope"] {
        assert_eq!(rendezvous_base_from(env_of(&[("EVE_LINK_RENDEZVOUS", on)])), RENDEZVOUS_URL, "{on:?}");
    }
    assert_eq!(rendezvous_base_from(env_of(&[("EVE_LINK_RENDEZVOUS_URL", "")])), RENDEZVOUS_URL);
    assert_eq!(rendezvous_base_from(env_of(&[("EVE_LINK_RENDEZVOUS_URL", "  ")])), "");
    assert_eq!(rendezvous_base_from(env_of(&[("EVE_LINK_RENDEZVOUS_URL", " https://n.example/// ")])), "https://n.example");
}

/// Python's own answers for the same inputs, when python3 is on PATH. Its module needs only the standard library.
#[test]
fn rust_and_python_agree() {
    let bridge = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/src");
    let dir = tmp();
    let rust_file = dir.path().join("rust.json");
    let rust_token = LinkToken::new(&rust_file).ensure().unwrap();
    let script = r#"
import json, sys
sys.path.insert(0, sys.argv[1])
import importlib.util
spec = importlib.util.spec_from_file_location("stick_link", sys.argv[1] + "/eve_bridge/stick_link.py")
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
from pathlib import Path
py_token = m.LinkToken(Path(sys.argv[2]) / "py.json").ensure()
tokens = ["a" * 40, "tok_en-123", "é/ #?&", ""]
out = {
  "consts": [m.PAIR_PATH, m.CALL_PATH, m.UPDATE_TEXT, m.PAIR_LINE, m.NEW_LINE, m.OFF_LINE, m.RENDEZVOUS_URL,
             m.RENDEZVOUS_LABEL.decode(), m.REPOST_SECS, m.RENDEZVOUS_TIMEOUT_SECS, m.PAIR_CSP, list(m.PAIR_HEADERS.items())],
  "topics": [m.rendezvous_topic(t) for t in tokens],
  "pair": [m.pair_url(u, t) for u in ["https://x.trycloudflare.com///", "", "http://h"] for t in tokens],
  "rust_seen_by_python": m.LinkToken(Path(sys.argv[2]) / "rust.json").current(),
  "py_token": py_token,
}
print(json.dumps(out))
"#;
    let output = match Command::new("python3").args(["-I", "-c", script, bridge]).arg(dir.path()).output() {
        Ok(o) if o.status.success() => o,
        Ok(o) => panic!("python3 failed: {}", String::from_utf8_lossy(&o.stderr)),
        Err(_) => {
            eprintln!("python3 not found; skipping the cross-check");
            return;
        }
    };
    let py: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let consts = serde_json::json!([
        PAIR_PATH, CALL_PATH, UPDATE_TEXT, PAIR_LINE, NEW_LINE, OFF_LINE, RENDEZVOUS_URL,
        std::str::from_utf8(RENDEZVOUS_LABEL).unwrap(), REPOST_SECS, RENDEZVOUS_TIMEOUT_SECS, PAIR_CSP,
        PAIR_HEADERS.iter().map(|(k, v)| [k, v]).collect::<Vec<_>>()
    ]);
    assert_eq!(py["consts"], consts);
    let tokens = ["a".repeat(40), "tok_en-123".into(), "é/ #?&".into(), String::new()];
    let topics: Vec<String> = tokens.iter().map(|t| rendezvous_topic(t)).collect();
    assert_eq!(py["topics"], serde_json::json!(topics));
    let pairs: Vec<String> = ["https://x.trycloudflare.com///", "", "http://h"]
        .iter()
        .flat_map(|u| tokens.iter().map(move |t| pair_url(u, t)))
        .collect();
    assert_eq!(py["pair"], serde_json::json!(pairs));
    assert_eq!(py["rust_seen_by_python"], serde_json::json!(rust_token));
    let py_token = py["py_token"].as_str().unwrap();
    assert_eq!(py_token.len(), 43);
    assert_eq!(LinkToken::new(dir.path().join("py.json")).current(), py_token);
    assert!(LinkToken::new(dir.path().join("py.json")).check(py_token));
}

// ---- the real HTTP calls, against a loopback server ---------------------------------------------------------

struct Seen {
    request_line: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

/// Answers each connection with the next canned reply and reports what it was sent.
fn serve(replies: Vec<String>) -> (String, mpsc::Receiver<Seen>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for reply in replies {
            let Ok((mut stream, _)) = listener.accept() else { return };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            let mut headers = Vec::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let line = line.trim_end();
                if line.is_empty() {
                    break;
                }
                let (k, v) = line.split_once(':').unwrap();
                headers.push((k.trim().to_ascii_lowercase(), v.trim().to_owned()));
            }
            let len: usize = headers
                .iter()
                .find(|(k, _)| k == "content-length")
                .map(|(_, v)| v.parse().unwrap())
                .unwrap_or(0);
            let mut body = vec![0; len];
            reader.read_exact(&mut body).unwrap();
            stream.write_all(reply.as_bytes()).unwrap();
            let _ = tx.send(Seen { request_line: request_line.trim_end().to_owned(), headers, body });
        }
    });
    (base, rx)
}

fn reply(status: &str, extra: &str, body: &str) -> String {
    format!("HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
}

#[test]
fn post_sends_the_address_as_plain_text_titled_eve_and_returns_the_status() {
    let (base, rx) = serve(vec![reply("200 OK", "", "{}"), reply("500 Internal Server Error", "", "no")]);
    let token = "a".repeat(40);
    assert!(publish_address("https://x.trycloudflare.com/", &token, &base));
    let seen = rx.recv().unwrap();
    assert_eq!(seen.request_line, format!("POST /{} HTTP/1.1", rendezvous_topic(&token)));
    assert!(seen.headers.contains(&("content-type".into(), "text/plain".into())));
    assert!(seen.headers.contains(&("title".into(), "eve".into())));
    assert_eq!(seen.body, b"https://x.trycloudflare.com");
    assert_eq!(post(&format!("{base}/t"), b"x"), Ok(500)); // an error status is the answer, not an error
}

#[test]
fn post_does_not_follow_a_307_and_a_bad_url_is_a_value_error() {
    let (base, _rx) = serve(vec![reply("307 Temporary Redirect", "Location: /elsewhere\r\n", "")]);
    assert_eq!(post(&format!("{base}/t"), b"x"), Ok(307));
    assert_eq!(post("ntfy.sh/t", b"x"), Err(TransportError::new("ValueError")));
    // nothing listens on port 9 of loopback (discard): a connection error, never a panic
    assert!(post("http://127.0.0.1:9/t", b"x").is_err());
}

#[test]
fn latest_address_reads_the_topic_over_http() {
    let token = "t".repeat(43);
    let body = String::from_utf8(ndjson(&["https://one.trycloudflare.com", "https://two.trycloudflare.com"])).unwrap();
    let (base, rx) = serve(vec![reply("200 OK", "", &body), reply("404 Not Found", "", "{}")]);
    assert_eq!(latest_address(&token, &format!("{base}/")), "https://two.trycloudflare.com");
    let seen = rx.recv().unwrap();
    assert_eq!(seen.request_line, format!("GET /{}/json?poll=1&since=all HTTP/1.1", rendezvous_topic(&token)));
    assert_eq!(latest_address(&token, &base), ""); // an error status reads as nothing
    assert_eq!(latest_address(&token, "ntfy.sh"), "");
}

#[test]
fn redirects_follow_urllibs_rules() {
    // urllib follows ten redirects to distinct targets and refuses the eleventh with its code.
    let mut ten: Vec<String> = (1..=10).map(|i| reply("302 Found", &format!("Location: /r{i}\r\n"), "")).collect();
    ten.push(reply("200 OK", "", "ok"));
    let (base, rx) = serve(ten);
    assert_eq!(post(&format!("{base}/t"), b"x"), Ok(200));
    let seen: Vec<Seen> = rx.iter().take(11).collect();
    assert_eq!(seen[0].request_line, "POST /t HTTP/1.1");
    assert_eq!(seen[10].request_line, "GET /r10 HTTP/1.1"); // a 302 turns a POST into a GET, as urllib does
    assert!(seen[1..].iter().all(|s| s.body.is_empty() && !s.headers.iter().any(|(k, _)| k == "referer")));
    let eleven: Vec<String> = (1..=11).map(|i| reply("302 Found", &format!("Location: /r{i}\r\n"), "")).collect();
    let (base, _rx) = serve(eleven);
    assert_eq!(post(&format!("{base}/t"), b"x"), Ok(302));
    // the same target four times is followed; the fifth is refused
    let same: Vec<String> = (0..5).map(|_| reply("302 Found", "Location: /same\r\n", "")).collect();
    let (base, _rx) = serve(same);
    assert_eq!(post(&format!("{base}/t"), b"x"), Ok(302));
    // a GET follows a 307; a redirect that is refused reads as nothing
    let token = "t".repeat(43);
    let body = String::from_utf8(ndjson(&["https://one.trycloudflare.com"])).unwrap();
    let (base, _rx) = serve(vec![reply("307 Temporary Redirect", "Location: /moved\r\n", ""), reply("200 OK", "", &body)]);
    assert_eq!(latest_address(&token, &base), "https://one.trycloudflare.com");
    let (base, _rx) = serve(vec![reply("302 Found", "Location: gopher://h/x\r\n", "")]);
    assert_eq!(latest_address(&token, &base), "");
}

#[test]
fn json_is_read_as_pythons_json_loads_reads_it() {
    let token = "t".repeat(43);
    let body = concat!(
        "{\"event\":\"message\",\"message\":\"https://a.trycloudflare.com\",\"time\":1e400}\n",
        "{\"event\":\"open\",\"event\":\"message\",\"message\":\"https://x.trycloudflare.com\",\"message\":\"https://b.trycloudflare.com\"}\n",
    );
    assert_eq!(latest_address_with(&token, "https://n", |_, _| Ok(body.as_bytes().to_vec())), "https://b.trycloudflare.com");
    // `$` before one final newline: "…com\n/" stripped of its slash still matches, and is kept as it is
    let body = "{\"event\":\"message\",\"message\":\"https://c.trycloudflare.com\\n/\"}\n";
    assert_eq!(latest_address_with(&token, "https://n", |_, _| Ok(body.as_bytes().to_vec())), "https://c.trycloudflare.com\n");
    let body = "{\"event\":\"message\",\"message\":\"https://c.trycloudflare.com\\n\\n/\"}\n";
    assert_eq!(latest_address_with(&token, "https://n", |_, _| Ok(body.as_bytes().to_vec())), "");
    let dir = tmp();
    let path = dir.path().join("stick-link.json");
    let t = LinkToken::new(&path);
    fs::write(&path, format!("{{\"token\": \"{token}\", \"created\": 1e400}}")).unwrap();
    assert_eq!(t.current(), token);
    fs::write(&path, format!("{{\"token\": 5, \"token\": \"{token}\"}}")).unwrap();
    assert_eq!(t.current(), token);
    fs::write(&path, format!("{{\"token\": \"{token}\"}} x")).unwrap();
    assert_eq!(t.current(), "");
}

#[test]
#[ignore = "live: reads an empty topic on ntfy.sh over TLS"]
fn live_the_real_rendezvous_answers_over_tls() {
    let token = "live-check-".repeat(4);
    let url = format!("{RENDEZVOUS_URL}/{}/json?poll=1&since=all", rendezvous_topic(&token));
    assert!(open(&url, Duration::from_secs(10)).is_ok());
    assert_eq!(latest_address(&token, RENDEZVOUS_URL), "");
}
