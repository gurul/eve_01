//! Helpers the Python tests had as fixtures: a temporary memory root, the fake dream client, a captured log, a
//! scoped environment, git run by hand, and a one-shot HTTP server standing in for OpenRouter.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::recall::RecallConfig;
use crate::records::{self, DreamClient, Record, Seams};

pub fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

/// `cfg_at(tmp_path)`: the memory root made, the records folder not yet.
pub fn cfg_at(tmp_path: &Path) -> RecallConfig {
    let store = tmp_path.join("memory");
    std::fs::create_dir_all(&store).unwrap();
    RecallConfig::new(store, tmp_path.join("notes"))
}

pub fn rdir(cfg: &RecallConfig) -> PathBuf {
    records::records_dir(cfg)
}

pub fn seed(cfg: &RecallConfig, recs: &[&Record]) {
    std::fs::create_dir_all(rdir(cfg)).unwrap();
    for rec in recs {
        std::fs::write(rdir(cfg).join(format!("{}.md", rec.id)), records::render_record(rec)).unwrap();
    }
}

pub fn put(cfg: &RecallConfig, name: &str, text: &str) -> PathBuf {
    std::fs::create_dir_all(rdir(cfg)).unwrap();
    let path = rdir(cfg).join(name);
    std::fs::write(&path, text).unwrap();
    path
}

pub fn read(path: impl AsRef<Path>) -> String {
    std::fs::read_to_string(path).unwrap()
}

/// `git -C <records> ...` → stdout, run outside the crate's guard (as the Python tests' subprocess.run).
pub fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

pub fn git_ok(dir: &Path, args: &[&str]) -> bool {
    Command::new("git").arg("-C").arg(dir).args(args).output().unwrap().status.success()
}

pub fn rgit(cfg: &RecallConfig, args: &[&str]) -> String {
    git_in(&rdir(cfg), args)
}

/// The process environment less the two record switches (Python's autouse `switches_off`), plus `extra`.
pub fn env_with(extra: &[(&str, &str)]) -> HashMap<String, String> {
    let mut env: HashMap<String, String> = crate::py::environ();
    env.remove(records::VALIDITY_ENV);
    env.remove(records::SOURCES_ENV);
    env.remove("EVE_PROFILE");
    for (k, v) in extra {
        env.insert(k.to_string(), v.to_string());
    }
    env
}

/// Run `f` with the switches as `extra` sets them and nothing else changed.
pub fn with_env<R>(extra: &[(&str, &str)], f: impl FnOnce() -> R) -> R {
    records::with_seams(Seams { env: Some(env_with(extra)), ..Seams::default() }, f)
}

/// One reply the fake client gives.
pub enum Reply {
    Json(Value),
    Text(String),
    Fail(String),
}

impl From<Value> for Reply {
    fn from(v: Value) -> Self {
        Reply::Json(v)
    }
}

/// The dream's client: reconcile(body) and consolidate(body) → JSON text, recording each body.
#[derive(Default)]
pub struct FakeClient {
    results: RefCell<VecDeque<Reply>>,
    pub bodies: RefCell<Vec<(String, String)>>,
}

impl FakeClient {
    pub fn new(results: Vec<Reply>) -> Self {
        Self { results: RefCell::new(results.into()), bodies: RefCell::new(Vec::new()) }
    }

    pub fn one(v: Value) -> Self {
        Self::new(vec![Reply::Json(v)])
    }

    fn next(&self, method: &str, body: &str) -> anyhow::Result<String> {
        self.bodies.borrow_mut().push((method.to_string(), body.to_string()));
        match self.results.borrow_mut().pop_front().expect("a reply for every call") {
            Reply::Json(v) => Ok(serde_json::to_string(&v).unwrap()),
            Reply::Text(t) => Ok(t),
            Reply::Fail(msg) => Err(anyhow::anyhow!(msg)),
        }
    }
}

impl DreamClient for FakeClient {
    fn reconcile(&self, body: &str) -> anyhow::Result<String> {
        self.next("reconcile", body)
    }

    fn consolidate(&self, body: &str) -> anyhow::Result<String> {
        self.next("consolidate", body)
    }
}

/// Run `f` with every log line this thread writes at `level` and above captured; → the captured text.
///
/// One global subscriber at TRACE writes into a per-thread buffer while a capture is on, so every callsite stays
/// interesting whatever other tests run at the same time.
pub fn capture_logs(level: tracing::Level, f: impl FnOnce()) -> String {
    thread_local! {
        static SINK: RefCell<Option<(tracing::Level, Vec<u8>)>> = const { RefCell::new(None) };
    }
    struct ThreadSink;
    impl Write for ThreadSink {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            SINK.with(|s| {
                if let Some((level, buf)) = s.borrow_mut().as_mut() {
                    let text = String::from_utf8_lossy(b);
                    let shown = ["ERROR", "WARN", "INFO", "DEBUG", "TRACE"]
                        .iter()
                        .take_while(|l| **l != level.as_str())
                        .chain(std::iter::once(&level.as_str()))
                        .any(|l| text.contains(&format!(" {l} ")));
                    if shown {
                        buf.extend_from_slice(b);
                    }
                }
            });
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        let sub = tracing_subscriber::fmt()
            .with_writer(|| ThreadSink)
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .finish();
        let _ = tracing::subscriber::set_global_default(sub);
    });
    SINK.with(|s| *s.borrow_mut() = Some((level, Vec::new())));
    f();
    let got = SINK.with(|s| s.borrow_mut().take()).map(|(_, b)| b).unwrap_or_default();
    String::from_utf8_lossy(&got).into_owned()
}

/// A local stand-in for OpenRouter: answers every POST with a Responses reply whose text is `text`, and keeps each
/// request's path and JSON body.
pub struct FakeServer {
    pub base_url: String,
    pub requests: Arc<Mutex<Vec<(String, Value)>>>,
}

pub fn fake_openrouter(text: &str) -> FakeServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let requests: Arc<Mutex<Vec<(String, Value)>>> = Arc::default();
    let seen = requests.clone();
    let reply = serde_json::json!({
        "id": "test", "object": "response", "created_at": 0, "status": "completed", "model": "m",
        "output": [{"id": "message", "type": "message", "role": "assistant", "status": "completed",
                    "content": [{"type": "output_text", "text": text, "annotations": []}]}],
        "usage": {"input_tokens": 1000, "output_tokens": 100, "total_tokens": 1100}});
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            if reader.read_line(&mut request_line).is_err() {
                continue;
            }
            let path = request_line.split_whitespace().nth(1).unwrap_or("").to_string();
            let mut length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                let lower = line.to_ascii_lowercase();
                if let Some(v) = lower.strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; length];
            let _ = reader.read_exact(&mut body);
            let json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            seen.lock().unwrap().push((path, json));
            let payload = serde_json::to_vec(&reply).unwrap();
            let mut stream = stream;
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                payload.len()
            );
            let _ = stream.write_all(&payload);
            let _ = stream.flush();
        }
    });
    FakeServer { base_url, requests }
}
