//! `tests/test_america_source_guard.py`: the additional source restriction covers the complete guarded HTTP
//! exchange (eve-web's `watch.http_request` with `url_guard`, the reader Alexandria's page reads go through).

use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, TcpListener};
use std::sync::Arc;

use eve_web::watch::net::{http_request, AddressRules, HttpOptions, UrlGuard};
use parking_lot::Mutex;

/// The loopback test server passes both address checks (Python's `loopback_is_public` fixture).
fn loopback_is_public() -> AddressRules {
    AddressRules { resolve: Some(Arc::new(|_: &str| true)), ip_public: Arc::new(|_: IpAddr| true), ..AddressRules::system() }
}

/// `Pages`: /redirect-ok and /redirect-bad answer 302 to /ok or /outside; anything else is "official source".
fn serve(hits: Arc<Mutex<Vec<String>>>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() {
                continue;
            }
            let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
                    break;
                }
            }
            hits.lock().push(path.clone());
            let reply = if path.starts_with("/redirect") {
                let to = if path.ends_with("ok") { "/ok" } else { "/outside" };
                format!("HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            } else {
                "HTTP/1.1 200 OK\r\nContent-Length: 15\r\nConnection: close\r\n\r\nofficial source".to_string()
            };
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    port
}

fn opts(guard: Option<UrlGuard>) -> HttpOptions {
    HttpOptions { timeout: 2.0, url_guard: guard, rules: Some(loopback_is_public()), ..HttpOptions::default() }
}

#[test]
fn test_source_guard_checks_initial_and_redirect_urls_with_real_exchange() {
    let hits: Arc<Mutex<Vec<String>>> = Arc::default();
    let base = format!("http://127.0.0.1:{}", serve(hits.clone()));
    let checked: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = checked.clone();
    let guard: UrlGuard = Arc::new(move |url: &str| {
        seen.lock().push(url.to_string());
        let path = eve_web::watch::net::urlsplit(url).map(|p| p.path).unwrap_or_default();
        if path == "/outside" {
            "outside source restriction".to_string()
        } else {
            String::new()
        }
    });
    assert_eq!(
        http_request(&format!("{base}/redirect-ok"), &opts(Some(guard.clone()))).unwrap(),
        (200, "official source".to_string())
    );
    assert_eq!(*checked.lock(), vec![format!("{base}/redirect-ok"), format!("{base}/ok")]);
    let err = http_request(&format!("{base}/redirect-bad"), &opts(Some(guard.clone()))).unwrap_err();
    assert!(err.reason.contains("outside source restriction"));
    assert!(!hits.lock().contains(&"/outside".to_string()));
    let err = http_request(&format!("{base}/outside"), &opts(Some(guard))).unwrap_err();
    assert!(err.reason.contains("outside source restriction"));
    assert!(!hits.lock().contains(&"/outside".to_string()));
    // The existing public-reader behavior remains when no extra guard is supplied; this is also the positive
    // control for the excluded target.
    assert_eq!(http_request(&format!("{base}/redirect-bad"), &opts(None)).unwrap(), (200, "official source".to_string()));
    assert!(hits.lock().contains(&"/outside".to_string()));
}

#[test]
fn test_source_guard_supplements_public_dns_check() {
    let checked: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = checked.clone();
    // check_url refuses the host (Python patched check_url itself to say "private address")
    let rules = AddressRules {
        resolve: Some(Arc::new(move |host: &str| {
            seen.lock().push(host.to_string());
            false
        })),
        ..AddressRules::system()
    };
    let guard: UrlGuard = Arc::new(|_: &str| String::new());
    let options = HttpOptions { timeout: 2.0, url_guard: Some(guard), rules: Some(rules), ..HttpOptions::default() };
    let err = http_request("https://agency.gov/", &options).unwrap_err();
    assert!(err.reason.contains("private"), "{}", err.reason);
    assert_eq!(*checked.lock(), vec!["agency.gov".to_string()]);
}
