//! test_phone_location.py's log checks: the phone's coordinates never reach the log, and the filter only touches
//! Open-Meteo URLs. A real reqwest client talks to a local server standing in for api.open-meteo.com (nothing
//! leaves the Mac), and the daemon's INFO log is captured. Its own test binary: it takes the global filter off for
//! the positive control.

use std::collections::HashMap;
use std::io::Write as _;
use std::net::SocketAddr;
use std::sync::Arc;

use chrono::{Duration, NaiveDate, Utc};
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use eve_places::phone_location as pl;
use eve_quick_answers as qa;

#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Log {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Log {
    fn httpx_lines(&self) -> Vec<String> {
        String::from_utf8_lossy(&self.0.lock()).lines().filter(|l| l.contains("httpx")).map(str::to_string).collect()
    }

    fn clear(&self) {
        self.0.lock().clear();
    }
}

const BODY: &str = r#"{"current": {"temperature_2m": 60, "apparent_temperature": 60, "weather_code": 0, "wind_speed_10m": 3},
 "daily": {"temperature_2m_min": [50, 50], "temperature_2m_max": [70, 70], "precipitation_probability_max": [0, 0],
           "weather_code": [0, 0]}}"#;

/// A one-route HTTP server: every request gets the forecast; its request lines are kept.
async fn serve(seen: Arc<Mutex<Vec<String>>>) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { return };
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    let Ok(n) = sock.read(&mut chunk).await else { return };
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let head = String::from_utf8_lossy(&buf);
                seen.lock().push(head.lines().next().unwrap_or_default().to_string());
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{BODY}",
                    BODY.len()
                );
                let _ = sock.write_all(reply.as_bytes()).await;
            });
        }
    });
    addr
}

fn client(addr: SocketAddr) -> reqwest::Client {
    reqwest::Client::builder().resolve("api.open-meteo.com", addr).no_proxy().build().unwrap()
}

/// The real URL, sent to the local stand-in (plain HTTP on its port).
fn local(url: &str, addr: SocketAddr) -> String {
    url.replace("https://api.open-meteo.com", &format!("http://api.open-meteo.com:{}", addr.port()))
}

#[test]
fn test_the_phones_coordinates_never_reach_the_log() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("phone-location.json");
    let at = Utc::now() - Duration::minutes(5);
    pl::store(&pl::Fix::new(37.774912, -122.419412, 10.0, at, Some("San Francisco, CA")), Some(&file)).unwrap();
    let env: HashMap<String, String> = HashMap::from([(pl::FILE_ENV.to_string(), file.to_string_lossy().into_owned())]);
    let noon = NaiveDate::from_ymd_opt(2026, 10, 12).unwrap().and_hms_opt(12, 0, 0).unwrap();

    let log = Log::default();
    let writer = log.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .finish();
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();

    tracing::subscriber::with_default(subscriber, || {
        let addr = rt.block_on(serve(seen.clone()));
        let get = qa::getter(move |url: String, params: Map<String, Value>| async move {
            qa::get_json_with(&client(addr), &local(&url, addr), &params).await
        });
        let q = qa::QuickAnswers::new(Some(env.clone())).with_get(get).with_now(move |_| noon);
        let a = q.match_text("what's the weather").unwrap();
        let line = rt.block_on(q.answer(&a)).unwrap();
        assert!(line.starts_with("In San Francisco, CA it's"), "{line}");
        assert!(seen.lock().last().unwrap().contains("latitude=37.77")); // Open-Meteo was asked with them
        let lines = log.httpx_lines();
        assert!(
            lines.iter().any(|m| m.contains("api.open-meteo.com")
                && m.contains(&format!("latitude={}", qa::HIDDEN))
                && m.contains(&format!("longitude={}", qa::HIDDEN))
                && m.contains("\"HTTP/1.1 200 OK\"")),
            "{lines:?}"
        );
        assert!(!lines.iter().any(|m| m.contains("37.77") || m.contains("-122.4")), "{lines:?}");

        // positive control: with the filter taken off, the same answer does put the coordinates in the log
        log.clear();
        qa::remove_coordinate_filter();
        let bare = qa::getter(move |url: String, params: Map<String, Value>| async move {
            qa::fetch_json(&client(addr), &local(&url, addr), &params).await // without re-installing the filter
        });
        let q = qa::QuickAnswers::new(Some(env.clone())).with_get(bare).with_now(move |_| noon);
        // QuickAnswers::new installs the filter, as Python's import does: take it off again for the control
        qa::remove_coordinate_filter();
        let a = q.match_text("what's the weather").unwrap();
        rt.block_on(q.answer(&a)).unwrap();
        let leaked = log.httpx_lines().iter().any(|m| m.contains("latitude=37.77"));
        qa::hide_coordinates();
        assert!(leaked, "{:?}", log.httpx_lines());
    });
    let _ = std::io::stdout().flush();
}

#[test]
fn test_the_filter_only_touches_open_meteo_urls() {
    let f = qa::CoordinateFilter;
    let other = "HTTP Request: GET https://example.com/?latitude=1";
    assert_eq!(f.filter(other), other);
    assert!(f.filter(other).ends_with("latitude=1"));
    let meteo = "HTTP Request: GET https://api.open-meteo.com/v1/forecast?latitude=0.0&longitude=-0.5&x=1 \"200\"";
    assert_eq!(
        f.filter(meteo),
        "HTTP Request: GET https://api.open-meteo.com/v1/forecast?latitude=<hidden>&longitude=<hidden>&x=1 \"200\""
    );
    let _ = json!(null);
}
