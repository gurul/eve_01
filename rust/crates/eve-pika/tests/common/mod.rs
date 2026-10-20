//! The fakes the Python tests use (bridge/tests/test_pika.py): the car's TCP relay and its MJPEG camera, on loopback.
//!
//! The fake car speaks what github.com/gurul/pika's camera board does (firmware/camera_s3/camera_s3.ino): one TCP
//! client at a time, `{Heartbeat}` from the board, `{ok}` from the UNO for a stop, and nothing at all to a second
//! client while the first is connected. Its video is ESP-IDF httpd's chunked multipart (app_httpd.cpp
//! `stream_handler`).
#![allow(dead_code)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use eve_pika::pika::{self, Body, Pika, PikaConfig, PikaOptions};
use futures::FutureExt;
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader, DuplexStream};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

#[derive(Default)]
struct CarState {
    clients: Vec<Vec<u8>>,
    open: Vec<bool>,
    hangups: Vec<Arc<Notify>>,
}

/// The camera board's TCP relay on a loopback port. `busy`: it accepts and never answers (another controller
/// has the slot). Records every byte each client sent, and whether each connection is still open.
pub struct FakeCar {
    pub port: u16,
    state: Arc<Mutex<CarState>>,
    accept: JoinHandle<()>,
}

impl FakeCar {
    pub async fn start(busy: bool, beat: bool) -> FakeCar {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = Arc::new(Mutex::new(CarState::default()));
        let st = state.clone();
        let accept = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let hangup = Arc::new(Notify::new());
                let i = {
                    let mut s = st.lock();
                    s.clients.push(Vec::new());
                    s.open.push(true);
                    s.hangups.push(hangup.clone());
                    s.clients.len() - 1
                };
                tokio::spawn(client(stream, i, busy, beat, st.clone(), hangup));
            }
        });
        FakeCar { port, state, accept }
    }

    pub fn clients(&self) -> Vec<Vec<u8>> {
        self.state.lock().clients.clone()
    }

    pub fn open(&self) -> Vec<bool> {
        self.state.lock().open.clone()
    }

    /// The `{...}` frames client `i` sent, in order.
    pub fn frames(&self, i: usize) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let mut rest = self.state.lock().clients.get(i).cloned().unwrap_or_default();
        while let Some(j) = rest.iter().position(|b| *b == b'}') {
            out.push(rest[..=j].to_vec());
            rest = rest[j + 1..].to_vec();
        }
        out
    }

    pub fn drives(&self, i: usize) -> Vec<Vec<u8>> {
        self.frames(i).into_iter().filter(|f| contains(f, b"\"N\":2")).collect()
    }

    pub fn hang_up(&self, i: usize) {
        self.state.lock().hangups[i].notify_one();
    }

    pub fn close(&self) {
        self.accept.abort();
        for h in self.state.lock().hangups.iter() {
            h.notify_one();
        }
    }
}

pub fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

async fn client(mut stream: TcpStream, i: usize, busy: bool, beat: bool, st: Arc<Mutex<CarState>>, hangup: Arc<Notify>) {
    if beat && !busy {
        let _ = stream.write_all(b"{Heartbeat}").await;
    }
    let mut buf: Vec<u8> = Vec::new();
    let mut data = [0u8; 1024];
    loop {
        tokio::select! {
            _ = hangup.notified() => break,
            r = stream.read(&mut data) => {
                let n = match r { Ok(0) | Err(_) => break, Ok(n) => n };
                st.lock().clients[i].extend_from_slice(&data[..n]);
                buf.extend_from_slice(&data[..n]);
                while let Some(j) = buf.iter().position(|b| *b == b'}') {
                    let f = buf[..=j].to_vec();
                    buf = buf[j + 1..].to_vec();
                    if !busy && contains(&f, b"\"N\":100") {
                        let _ = stream.write_all(b"{ok}").await; // the UNO's answer to a stop (no tag)
                    }
                }
            }
        }
    }
    st.lock().open[i] = false;
}

/// The camera's MJPEG server: ESP-IDF httpd, chunked, one viewer at a time (a second waits, unanswered).
pub struct FakeCamera {
    pub port: u16,
    pub viewers: Arc<AtomicUsize>,
    pub ended: Arc<AtomicUsize>,
}

impl FakeCamera {
    pub async fn start(frames: Vec<Vec<u8>>) -> FakeCamera {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let viewers = Arc::new(AtomicUsize::new(0));
        let ended = Arc::new(AtomicUsize::new(0));
        let busy = Arc::new(tokio::sync::Mutex::new(()));
        let (v, e) = (viewers.clone(), ended.clone());
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(camera_client(stream, frames.clone(), busy.clone(), v.clone(), e.clone()));
            }
        });
        FakeCamera { port, viewers, ended }
    }

    pub fn ended(&self) -> usize {
        self.ended.load(Ordering::SeqCst)
    }

    pub fn viewers(&self) -> usize {
        self.viewers.load(Ordering::SeqCst)
    }
}

async fn camera_client(
    stream: TcpStream,
    frames: Vec<Vec<u8>>,
    busy: Arc<tokio::sync::Mutex<()>>,
    viewers: Arc<AtomicUsize>,
    ended: Arc<AtomicUsize>,
) {
    let mut reader = BufReader::new(stream);
    if pika::read_until(&mut reader, b"\r\n\r\n", 1 << 16).await.is_err() {
        return;
    }
    let mut stream = reader.into_inner();
    let _guard = busy.lock().await;
    viewers.fetch_add(1, Ordering::SeqCst);
    let sent: std::io::Result<()> = async {
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: multipart/x-mixed-replace;boundary=123456789000000000000987654321\
\r\nTransfer-Encoding: chunked\r\n\r\n",
            )
            .await?;
        let mut i = 0;
        loop {
            let jpeg = &frames[i % frames.len()];
            let mut part = format!(
                "\r\n--123456789000000000000987654321\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
                jpeg.len()
            )
            .into_bytes();
            part.extend_from_slice(jpeg);
            for piece in [&part[..7], &part[7..]] {
                // chunk edges inside a part
                let mut chunk = format!("{:x}\r\n", piece.len()).into_bytes();
                chunk.extend_from_slice(piece);
                chunk.extend_from_slice(b"\r\n");
                stream.write_all(&chunk).await?;
            }
            stream.flush().await?;
            i += 1;
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    .await;
    let _ = sent;
    ended.fetch_add(1, Ordering::SeqCst);
}

pub fn jpeg(n: u8) -> Vec<u8> {
    let mut out = b"\xff\xd8".to_vec();
    out.extend(std::iter::repeat_n(n, 40));
    out.extend_from_slice(b"\xff\xd9");
    out
}

pub fn opts() -> PikaOptions {
    PikaOptions { heartbeat_secs: 0.3, idle_secs: 30.0, hello_secs: 0.5, tick_secs: 0.02, ..Default::default() }
}

pub fn make_with(car: &FakeCar, video_port: u16, o: PikaOptions) -> Pika {
    let cfg = PikaConfig { host: "127.0.0.1".into(), port: car.port, video_port, ..Default::default() };
    Pika::with_options(cfg, o)
}

pub fn make(car: &FakeCar) -> Pika {
    make_with(car, 1, opts())
}

pub async fn drive(p: &Pika, d: &str, hold: &str) -> Value {
    p.panel(&json!({"action": "drive", "dir": d, "hold": hold})).await
}

pub async fn settle(secs: f64) {
    tokio::time::sleep(Duration::from_secs_f64(secs)).await;
}

pub fn refuse() -> pika::Opener {
    Arc::new(|_host: String, _port: u16| {
        async { Err(std::io::Error::from(std::io::ErrorKind::ConnectionRefused)) }.boxed()
    })
}

/// A viewer of the tunnel stream: `Pika::stream` runs into one end of a pipe, as the link door hands it the app's
/// socket; this reads the other end.
pub struct Viewer {
    pub task: JoinHandle<()>,
    pub reader: BufReader<DuplexStream>,
}

/// The app's side of the tunnel video: the status, at least `want` JPEGs, and the open connection.
pub async fn read_parts(p: &Pika, want: usize) -> (u16, Vec<Vec<u8>>, Viewer) {
    let (ours, theirs) = tokio::io::duplex(1 << 16);
    let car = p.clone();
    let task = tokio::spawn(async move {
        let mut w = theirs;
        car.stream(&mut w).await;
    });
    let mut reader = BufReader::new(ours);
    let head = tokio::time::timeout(Duration::from_secs(5), pika::read_until(&mut reader, b"\r\n\r\n", 1 << 16))
        .await
        .expect("head in time")
        .expect("head");
    let text = String::from_utf8_lossy(&head).to_string();
    let status: u16 = text.split_whitespace().nth(1).unwrap().parse().unwrap();
    assert!(text.contains("multipart/x-mixed-replace; boundary=frame"), "{text}");
    let mut got = Vec::new();
    let mut buf = Vec::new();
    {
        let mut body = Body::new(&mut reader, true);
        while let Ok(Some(data)) = body.next().await {
            buf.extend_from_slice(&data);
            let (found, rest) = pika::split_jpegs(&buf);
            buf = rest;
            got.extend(found);
            if got.len() >= want {
                break;
            }
        }
    }
    (status, got, Viewer { task, reader })
}
