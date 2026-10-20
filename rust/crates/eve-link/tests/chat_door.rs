//! link_chat.LinkChat (the app's door) and link_today.Today, ported from test_link_chat.py, test_link_today.py and
//! the door parts of test_link_grade.py, test_link_reactions.py, test_link_uploads.py, test_app_media.py,
//! test_obsbot_panel.py, test_pika.py, test_work_sessions.py and test_muse_pi.py.
//!
//! The door is served on a loopback port by a small stand-in for miniapp.MiniAppServer (it reads the request's head
//! as miniapp.py does and hands the rest to `LinkChat::serve`), and a raw HTTP client stands in for the app's
//! URLSession. The chat's brain, the chief, the Mini App server's cards and Work sessions are fakes of the seams.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use futures::future::BoxFuture;
use futures::FutureExt;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

use eve_link::link_chat::door::LinkChatOptions;
use eve_link::link_chat::{self, LinkChat};
use eve_link::link_today::{events_part, music_part, tv_part, Today, TodayOptions};
use eve_link::link_topics::{Bookmarks, Titler};
use eve_link::seams::{
    AppInfo, AppOut, AppStoreView, Chief, DirectApps, DynWrite, Headers, HearOptions, Hub, Inlet, InletFn, JobCard,
    JobPhase, MiniApp, Obj, WorkDoor,
};
use eve_link::updates::{SuggestClient, UpdateSources, Updates, UpdatesOptions};
use eve_link_reactions::Reactions;
use eve_link_uploads::{Upload, UploadStore};
use eve_transcripts::{Append, TranscriptConfig, Transcripts};
use eve_web::watch::Watch;

const JPEG: &[u8] = b"\
\xff\xd8\xff\xe0\x00\x10\x4a\x46\x49\x46\x00\x01\x01\x00\x00\x01\x00\x01\x00\x00\xff\xdb\x00\x43\x00\x08\x06\x06\
\x07\x06\x05\x08\x07\x07\x07\x09\x09\x08\x0a\x0c\x14\x0d\x0c\x0b\x0b\x0c\x19\x12\x13\x0f\x14\x1d\x1a\x1f\x1e\x1d\
\x1a\x1c\x1c\x20\x24\x2e\x27\x20\x22\x2c\x23\x1c\x1c\x28\x37\x29\x2c\x30\x31\x34\x34\x34\x1f\x27\x39\x3d\x38\x32\
\x3c\x2e\x33\x34\x32\xff\xdb\x00\x43\x01\x09\x09\x09\x0c\x0b\x0c\x18\x0d\x0d\x18\x32\x21\x1c\x21\x32\x32\x32\x32\
\x32\x32\x32\x32\x32\x32\x32\x32\x32\x32\x32\x32\x32\x32\x32\x32\x32\x32\x32\x32\x32\x32\x32\x32\x32\x32\x32\x32\
\x32\x32\x32\x32\x32\x32\x32\x32\x32\x32\x32\x32\x32\x32\x32\x32\x32\x32\xff\xc0\x00\x11\x08\x00\x30\x00\x40\x03\
\x01\x22\x00\x02\x11\x01\x03\x11\x01\xff\xc4\x00\x1f\x00\x00\x01\x05\x01\x01\x01\x01\x01\x01\x00\x00\x00\x00\x00\
\x00\x00\x00\x01\x02\x03\x04\x05\x06\x07\x08\x09\x0a\x0b\xff\xc4\x00\xb5\x10\x00\x02\x01\x03\x03\x02\x04\x03\x05\
\x05\x04\x04\x00\x00\x01\x7d\x01\x02\x03\x00\x04\x11\x05\x12\x21\x31\x41\x06\x13\x51\x61\x07\x22\x71\x14\x32\x81\
\x91\xa1\x08\x23\x42\xb1\xc1\x15\x52\xd1\xf0\x24\x33\x62\x72\x82\x09\x0a\x16\x17\x18\x19\x1a\x25\x26\x27\x28\x29\
\x2a\x34\x35\x36\x37\x38\x39\x3a\x43\x44\x45\x46\x47\x48\x49\x4a\x53\x54\x55\x56\x57\x58\x59\x5a\x63\x64\x65\x66\
\x67\x68\x69\x6a\x73\x74\x75\x76\x77\x78\x79\x7a\x83\x84\x85\x86\x87\x88\x89\x8a\x92\x93\x94\x95\x96\x97\x98\x99\
\x9a\xa2\xa3\xa4\xa5\xa6\xa7\xa8\xa9\xaa\xb2\xb3\xb4\xb5\xb6\xb7\xb8\xb9\xba\xc2\xc3\xc4\xc5\xc6\xc7\xc8\xc9\xca\
\xd2\xd3\xd4\xd5\xd6\xd7\xd8\xd9\xda\xe1\xe2\xe3\xe4\xe5\xe6\xe7\xe8\xe9\xea\xf1\xf2\xf3\xf4\xf5\xf6\xf7\xf8\xf9\
\xfa\xff\xc4\x00\x1f\x01\x00\x03\x01\x01\x01\x01\x01\x01\x01\x01\x01\x00\x00\x00\x00\x00\x00\x01\x02\x03\x04\x05\
\x06\x07\x08\x09\x0a\x0b\xff\xc4\x00\xb5\x11\x00\x02\x01\x02\x04\x04\x03\x04\x07\x05\x04\x04\x00\x01\x02\x77\x00\
\x01\x02\x03\x11\x04\x05\x21\x31\x06\x12\x41\x51\x07\x61\x71\x13\x22\x32\x81\x08\x14\x42\x91\xa1\xb1\xc1\x09\x23\
\x33\x52\xf0\x15\x62\x72\xd1\x0a\x16\x24\x34\xe1\x25\xf1\x17\x18\x19\x1a\x26\x27\x28\x29\x2a\x35\x36\x37\x38\x39\
\x3a\x43\x44\x45\x46\x47\x48\x49\x4a\x53\x54\x55\x56\x57\x58\x59\x5a\x63\x64\x65\x66\x67\x68\x69\x6a\x73\x74\x75\
\x76\x77\x78\x79\x7a\x82\x83\x84\x85\x86\x87\x88\x89\x8a\x92\x93\x94\x95\x96\x97\x98\x99\x9a\xa2\xa3\xa4\xa5\xa6\
\xa7\xa8\xa9\xaa\xb2\xb3\xb4\xb5\xb6\xb7\xb8\xb9\xba\xc2\xc3\xc4\xc5\xc6\xc7\xc8\xc9\xca\xd2\xd3\xd4\xd5\xd6\xd7\
\xd8\xd9\xda\xe2\xe3\xe4\xe5\xe6\xe7\xe8\xe9\xea\xf2\xf3\xf4\xf5\xf6\xf7\xf8\xf9\xfa\xff\xda\x00\x0c\x03\x01\x00\
\x02\x11\x03\x11\x00\x3f\x00\xe4\x28\xa2\x8a\xf1\x4f\xd3\x02\x8a\x28\xa0\x02\x8a\x28\xa0\x02\x8a\x28\xa0\x02\x8a\
\x28\xa0\x02\x8a\x28\xa0\x02\x8a\x28\xa0\x02\x8a\x28\xa0\x02\x8a\x28\xa0\x02\x8a\x28\xa0\x02\x8a\x28\xa0\x02\x8a\
\x28\xa0\x0f\xff\xd9";

const PDF: &[u8] = b"%PDF-1.4\n1 0 obj << >> endobj\ntrailer << >>\n%%EOF\n";

// ---- the client ------------------------------------------------------------------------------------------

/// A request body: none, JSON (as json.dumps writes it), or raw bytes.
enum Body {
    None,
    Json(Value),
    Raw(Vec<u8>),
}

struct Resp {
    status: u16,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

impl Resp {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| panic!("not JSON ({e}): {:?}", String::from_utf8_lossy(&self.body)))
    }
}

/// An HTTP/1.1 chunked body, checked as it is undone: every size line, every CRLF, and the 0 chunk at the end.
fn dechunk(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut rest = raw;
    loop {
        let at = rest.windows(2).position(|w| w == b"\r\n").expect("a chunk without its size line");
        let size = usize::from_str_radix(std::str::from_utf8(&rest[..at]).unwrap(), 16).unwrap();
        rest = &rest[at + 2..];
        if size == 0 {
            assert_eq!(rest, b"\r\n", "the stream must end with the 0 chunk");
            return out;
        }
        out.extend_from_slice(&rest[..size]);
        rest = &rest[size..];
        assert_eq!(&rest[..2], b"\r\n", "a chunk without its CRLF");
        rest = &rest[2..];
    }
}

async fn http(port: u16, method: &str, path: &str, body: Body, token: Option<&str>, origin: Option<&str>) -> Resp {
    let mut conn = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let data = match body {
        Body::None => Vec::new(),
        Body::Json(v) => serde_json::to_vec(&v).unwrap(),
        Body::Raw(b) => b,
    };
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n");
    if let Some(token) = token {
        head += &format!("Authorization: Bearer {token}\r\n");
    }
    if let Some(origin) = origin {
        head += &format!("Origin: {origin}\r\n");
    }
    if method == "POST" {
        head += &format!("Content-Type: application/json\r\nContent-Length: {}\r\n", data.len());
    }
    let mut out = head.into_bytes();
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(&data);
    conn.write_all(&out).await.unwrap();
    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(20), conn.read_to_end(&mut raw)).await.expect("the server closes").unwrap();
    parse(&raw)
}

fn parse(raw: &[u8]) -> Resp {
    let at = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap_or(raw.len());
    let top = String::from_utf8_lossy(&raw[..at]).to_string();
    let rest = if at + 4 <= raw.len() { &raw[at + 4..] } else { &[][..] };
    let mut lines = top.split("\r\n");
    let status: u16 = lines.next().unwrap().split(' ').nth(1).unwrap().parse().unwrap();
    let headers: HashMap<String, String> = lines
        .map(|ln| {
            let (k, v) = ln.split_once(':').unwrap_or((ln, ""));
            (k.trim().to_lowercase(), v.trim().to_string())
        })
        .collect();
    let body = if headers.get("transfer-encoding").map(String::as_str) == Some("chunked") { dechunk(rest) } else { rest.to_vec() };
    Resp { status, headers, body }
}

/// (each chunk's content, whether [DONE] came last, how many pings). Every event must be well formed.
fn events(body: &[u8]) -> (Vec<String>, bool, usize) {
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.ends_with("\n\n"), "{text:?}");
    let (mut contents, mut done, mut pings) = (Vec::new(), false, 0);
    for event in text[..text.len() - 2].split("\n\n") {
        assert!(!done, "nothing may follow [DONE]");
        if event == ": ping" {
            pings += 1;
        } else if event == "data: [DONE]" {
            done = true;
        } else {
            assert!(event.starts_with("data: ") && !event.contains('\n'), "{event:?}");
            let chunk: Value = serde_json::from_str(&event["data: ".len()..]).unwrap();
            contents.push(chunk["choices"][0]["delta"]["content"].as_str().unwrap().to_string());
            assert_eq!(chunk["choices"][0]["index"], 0);
        }
    }
    (contents, done, pings)
}

fn strs(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

// ---- the server ------------------------------------------------------------------------------------------

type TokenCheck = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// The Mini App server as the door reads it: its cards by name, the app builder's store and app_link.
#[derive(Default)]
struct FakeMini {
    hubs: Mutex<HashMap<String, Arc<dyn Hub>>>,
    store: Mutex<Option<Arc<dyn AppStoreView>>>,
    public_url: Mutex<Option<String>>,
}

impl FakeMini {
    fn set(&self, name: &str, hub: Arc<dyn Hub>) {
        self.hubs.lock().insert(name.to_string(), hub);
    }
}

impl MiniApp for FakeMini {
    fn hub(&self, name: &str) -> Option<Arc<dyn Hub>> {
        self.hubs.lock().get(name).cloned()
    }

    fn app_store(&self) -> Option<Arc<dyn AppStoreView>> {
        self.store.lock().clone()
    }

    fn app_link(&self, slug: &str, headers: &Headers) -> String {
        let origin = self
            .public_url
            .lock()
            .clone()
            .unwrap_or_else(|| format!("http://{}", headers.get("host").cloned().unwrap_or_default()));
        format!("{origin}/apps/{slug}/?t=fresh.{slug}")
    }
}

struct Server {
    port: u16,
    token: String,
    link: Arc<Mutex<Option<LinkChat>>>,
    check: Arc<Mutex<Option<TokenCheck>>>,
    mini: Arc<FakeMini>,
    tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl Server {
    async fn req(&self, method: &str, path: &str, body: Body) -> Resp {
        http(self.port, method, path, body, Some(&self.token), None).await
    }

    async fn get(&self, path: &str) -> Resp {
        self.req("GET", path, Body::None).await
    }

    async fn post(&self, path: &str, body: Value) -> Resp {
        self.req("POST", path, Body::Json(body)).await
    }

    async fn messages(&self, query: impl AsRef<str>) -> Value {
        let got = self.get(&format!("{}{}", link_chat::MESSAGES_PATH, query.as_ref())).await;
        assert_eq!(got.status, 200, "{}", String::from_utf8_lossy(&got.body));
        got.json()
    }

    async fn turn(&self, text: &str, client_id: &str) -> Resp {
        self.post(link_chat::TURN_PATH, json!({"text": text, "client_id": client_id})).await
    }

    /// `server.close()`: the door's streams end with [DONE], then the connections are cut.
    async fn close(&self) {
        let door = self.link.lock().clone();
        if let Some(door) = door {
            door.close();
            door.settle(link_chat::CLOSE_SETTLE_SECS).await;
        }
        let tasks: Vec<JoinHandle<()>> = self.tasks.lock().drain(..).collect();
        for t in tasks {
            t.abort();
        }
    }
}

/// The tmp-path stores every test door gets unless the test lends its own.
fn opts_at(tmp: &Path, mut opts: LinkChatOptions) -> LinkChatOptions {
    if opts.hidden.is_none() {
        opts.hidden = Some(Arc::new(link_chat::HiddenJobs::new(Some(&tmp.join("hidden-jobs.json")))));
    }
    if opts.uploads.is_none() {
        opts.uploads = Some(Arc::new(UploadStore::new(Some(tmp.join("up")))));
    }
    if opts.bookmarks.is_none() {
        opts.bookmarks = Some(Arc::new(Bookmarks::new(Some(&tmp.join("bm.json")))));
    }
    if opts.titler.is_none() {
        let env = HashMap::from([("EVE_TOPIC_TITLES".to_string(), "0".to_string())]);
        opts.titler = Some(Arc::new(Titler::new(Some(&tmp.join("titles.json")), Some(env), None)));
    }
    if opts.environ.is_none() {
        opts.environ = Some(env_at(tmp, &[]));
    }
    if opts.updates.is_none() {
        let env = HashMap::from([("EVE_UPDATES".to_string(), "0".to_string())]);
        opts.updates = Some(Arc::new(Updates::new(UpdatesOptions {
            path: Some(tmp.join("updates.json")),
            sources: Some(Arc::new(NoSources)),
            client: Some(Arc::new(|| None)),
            environ: Some(env),
            ..UpdatesOptions::default()
        })));
    }
    opts
}

/// phone_location's and places' files in the tmp folder, plus `extra`.
fn env_at(tmp: &Path, extra: &[(&str, &str)]) -> HashMap<String, String> {
    let mut env = HashMap::from([
        ("EVE_PHONE_LOCATION_FILE".to_string(), tmp.join("phone-location.json").display().to_string()),
        ("EVE_PLACES_FILE".to_string(), tmp.join("places.db").display().to_string()),
    ]);
    for (k, v) in extra {
        env.insert(k.to_string(), v.to_string());
    }
    env
}

async fn serve(tmp: &Path, inlet: Option<Arc<FakeInlet>>, opts: LinkChatOptions) -> Server {
    std::fs::create_dir_all(tmp).unwrap();
    let opts = opts_at(tmp, opts);
    let lent: InletFn = Arc::new(move || inlet.clone().map(|i| i as Arc<dyn Inlet>));
    let door = LinkChat::new(lent, opts);
    let token: String = format!("tok{}", "x".repeat(40));
    let want = token.clone();
    let check: TokenCheck = Arc::new(move |presented: &str| presented == want);
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = Server {
        port,
        token,
        link: Arc::new(Mutex::new(Some(door))),
        check: Arc::new(Mutex::new(Some(check))),
        mini: Arc::new(FakeMini::default()),
        tasks: Arc::new(Mutex::new(Vec::new())),
    };
    let (link, chk, mini, tasks) =
        (Arc::clone(&server.link), Arc::clone(&server.check), Arc::clone(&server.mini), Arc::clone(&server.tasks));
    let accept = tokio::spawn(async move {
        loop {
            let Ok((conn, _)) = listener.accept().await else { return };
            let (link, chk, mini) = (Arc::clone(&link), Arc::clone(&chk), Arc::clone(&mini));
            let task = tokio::spawn(handle(conn, link, chk, mini));
            let mut held = tasks.lock();
            held.retain(|t| !t.is_finished());
            held.push(task);
        }
    });
    server.tasks.lock().push(accept);
    server
}

/// miniapp.MiniAppServer._serve as far as `/api/link/`: the head read, names lowercased, the query split off.
async fn handle(conn: TcpStream, link: Arc<Mutex<Option<LinkChat>>>, check: Arc<Mutex<Option<TokenCheck>>>, mini: Arc<FakeMini>) {
    let (rd, mut wr) = conn.into_split();
    let mut rd = tokio::io::BufReader::new(rd);
    let mut lines: Vec<String> = Vec::new();
    loop {
        let mut line = Vec::new();
        match rd.read_until(b'\n', &mut line).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let text = String::from_utf8_lossy(&line).trim_end_matches(['\r', '\n']).to_string();
        if text.is_empty() {
            break;
        }
        lines.push(text);
    }
    let first: Vec<&str> = lines[0].splitn(3, ' ').collect();
    let (method, target) = (first.first().copied().unwrap_or(""), first.get(1).copied().unwrap_or(""));
    let headers: Headers = lines[1..]
        .iter()
        .map(|ln| {
            let (k, v) = ln.split_once(':').unwrap_or((ln, ""));
            (k.trim().to_lowercase(), v.trim().to_string())
        })
        .collect();
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let door = link.lock().clone();
    match door.filter(|_| path.starts_with("/api/link/")) {
        None => {
            let _ = wr
                .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\nContent-Length: 9\r\nConnection: close\r\n\r\nnot found")
                .await;
        }
        Some(door) => {
            let check = check.lock().clone();
            let apps: Arc<dyn MiniApp> = mini;
            let _ = door.serve(method, path, query, &headers, &mut rd, &mut wr, check.as_deref(), Some(apps)).await;
        }
    }
    let _ = wr.shutdown().await;
    // a body the door never read (a refusal) is read off before the socket goes, so the close is a FIN, not a reset
    // that could beat the answer to the client
    let drain = async {
        let mut buf = [0u8; 4096];
        while rd.read(&mut buf).await.is_ok_and(|n| n > 0) {}
    };
    let _ = tokio::time::timeout(Duration::from_secs(2), drain).await;
}

// ---- the chat's brain ------------------------------------------------------------------------------------

type Script = Arc<dyn Fn(Arc<FakeInlet>, String) -> BoxFuture<'static, ()> + Send + Sync>;

/// The inlet as link_chat uses it: hear, owner_busy, app_out, transcripts and the chat id. `script` runs as the
/// turn would, in a task, and calls app_out the way TelegramInlet._to_app does.
#[derive(Default)]
struct FakeInlet {
    me: Weak<FakeInlet>,
    off: AtomicBool,
    app_out: Mutex<Option<AppOut>>,
    transcripts: Mutex<Option<Arc<Transcripts>>>,
    heard: Mutex<Vec<(String, bool, Option<String>)>>,
    grades: Mutex<Vec<String>>,
    attachments: Mutex<Vec<Vec<Upload>>>,
    busy: AtomicBool,
    waiting: AtomicBool,
    script: Mutex<Option<Script>>,
    delivered: Mutex<Vec<bool>>,
    chief: Mutex<Option<Arc<dyn Chief>>>,
    filed: Mutex<HashMap<String, String>>,
    watches: Mutex<Option<Vec<Watch>>>,
    watches_break: AtomicBool,
    direct: Mutex<Option<Arc<dyn DirectApps>>>,
    reactions: Mutex<Option<Arc<Reactions>>>,
    muse: AtomicBool,
    link_uploads: Mutex<Option<Arc<UploadStore>>>,
    hear_fails: AtomicBool,
    said_in_hear: Mutex<Option<String>>,
}

impl FakeInlet {
    fn new(store: Option<Arc<Transcripts>>) -> Arc<Self> {
        Arc::new_cyclic(|me| FakeInlet { me: me.clone(), transcripts: Mutex::new(store), ..FakeInlet::default() })
    }

    fn with(store: Option<Arc<Transcripts>>, script: Script) -> Arc<Self> {
        let inlet = Self::new(store);
        *inlet.script.lock() = Some(script);
        inlet
    }

    /// One of eve's lines for app turn `app`, through app_out; whether a stream read it is kept in `delivered`.
    fn out(&self, app: &str, text: &str, kind: &str) {
        let out = self.app_out.lock().clone().expect("app_out is set");
        let read = out(app, text, kind);
        self.delivered.lock().push(read);
    }

    fn heard(&self) -> Vec<(String, bool, Option<String>)> {
        self.heard.lock().clone()
    }

    fn set_chief(&self, chief: Option<Arc<dyn Chief>>) {
        *self.chief.lock() = chief;
    }
}

impl Inlet for FakeInlet {
    fn chat_ready(&self) -> bool {
        !self.off.load(Ordering::SeqCst)
    }

    fn hear(&self, text: &str, opts: HearOptions) -> anyhow::Result<()> {
        if self.hear_fails.load(Ordering::SeqCst) {
            anyhow::bail!("the brain is down");
        }
        self.heard.lock().push((text.to_string(), opts.on_call, opts.app.clone()));
        self.grades.lock().push(opts.grade.clone());
        if !opts.attachments.is_empty() {
            self.attachments.lock().push(opts.attachments.clone());
        }
        let app = opts.app.clone().unwrap_or_default();
        if let Some(line) = self.said_in_hear.lock().clone() {
            self.out(&app, &line, "say"); // a quick answer said before hear returns
        }
        let script = self.script.lock().clone();
        if let Some(script) = script {
            self.busy.store(true, Ordering::SeqCst);
            let me = self.me.upgrade().expect("the inlet");
            tokio::spawn(script(me, app));
        }
        Ok(())
    }

    fn owner_busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    fn waiting_on_owner(&self) -> bool {
        self.waiting.load(Ordering::SeqCst)
    }

    fn app_filed(&self, turn_id: &str) -> Option<String> {
        self.filed.lock().get(turn_id).cloned()
    }

    fn set_app_out(&self, out: AppOut) {
        *self.app_out.lock() = Some(out);
    }

    fn set_link_uploads(&self, uploads: Arc<UploadStore>) {
        *self.link_uploads.lock() = Some(uploads);
    }

    fn transcripts(&self) -> Option<Arc<Transcripts>> {
        self.transcripts.lock().clone()
    }

    fn app_reactions(&self) -> Option<Arc<Reactions>> {
        self.reactions.lock().clone()
    }

    fn chief(&self) -> Option<Arc<dyn Chief>> {
        self.chief.lock().clone()
    }

    fn watches(&self) -> Option<Vec<Watch>> {
        if self.watches_break.load(Ordering::SeqCst) {
            panic!("the watcher's list could not be read");
        }
        self.watches.lock().clone()
    }

    fn direct_apps(&self) -> Option<Arc<dyn DirectApps>> {
        self.direct.lock().clone()
    }

    fn muse(&self) -> bool {
        self.muse.load(Ordering::SeqCst)
    }

    fn set_muse(&self, on: bool, tell: bool) {
        assert!(!tell, "the app's switch is quiet");
        self.muse.store(on, Ordering::SeqCst);
    }
}

fn store_at(tmp: &Path) -> Arc<Transcripts> {
    let store = Transcripts::new(TranscriptConfig::new(true, tmp.join("transcripts")), None, None);
    assert!(store.enabled);
    Arc::new(store)
}

fn script<F, Fut>(f: F) -> Script
where
    F: Fn(Arc<FakeInlet>, String) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    Arc::new(move |inlet, app| f(inlet, app).boxed())
}

fn two_lines() -> Script {
    script(|inlet: Arc<FakeInlet>, app: String| async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        inlet.out(&app, "First line.", "say");
        tokio::time::sleep(Duration::from_millis(20)).await;
        inlet.out(&app, "Second line.", "say");
        inlet.busy.store(false, Ordering::SeqCst);
    })
}

/// Busy and never done: it says one line and keeps the turn open.
fn forever() -> Script {
    script(|inlet: Arc<FakeInlet>, app: String| async move {
        inlet.out(&app, "Working on it.", "say");
    })
}

fn fast(settle: f64) -> LinkChatOptions {
    LinkChatOptions { settle_secs: settle, ..LinkChatOptions::default() }
}

struct FakeClock(Mutex<f64>);

impl FakeClock {
    fn new(t: f64) -> Arc<Self> {
        Arc::new(FakeClock(Mutex::new(t)))
    }
    fn add(&self, s: f64) {
        *self.0.lock() += s;
    }
    fn mono(self: &Arc<Self>) -> eve_link::Mono {
        let me = Arc::clone(self);
        Arc::new(move || *me.0.lock())
    }
}

// ---- who may call ----------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_the_app_with_the_link_token_gets_in() {
    let tmp = tempfile::tempdir().unwrap();
    let inlet = FakeInlet::new(Some(store_at(tmp.path())));
    let s = serve(tmp.path(), Some(Arc::clone(&inlet)), LinkChatOptions::default()).await;
    let path = link_chat::MESSAGES_PATH;
    assert_eq!(http(s.port, "GET", path, Body::None, None, None).await.status, 401); // no token
    assert_eq!(http(s.port, "GET", path, Body::None, Some(&"x".repeat(43)), None).await.status, 401); // a wrong one
    assert_eq!(http(s.port, "GET", path, Body::None, Some(&s.token[..s.token.len() - 1]), None).await.status, 401);
    let got = http(s.port, "GET", path, Body::None, Some(&s.token), Some("https://evil.example")).await;
    assert!(got.status == 403 && String::from_utf8_lossy(&got.body).contains("Eve Link app only")); // a web page
    assert_eq!(http(s.port, "GET", path, Body::None, Some(&s.token), Some("null")).await.status, 403);
    let hi = Body::Json(json!({"text": "hi"}));
    assert_eq!(http(s.port, "POST", link_chat::TURN_PATH, hi, Some(&s.token), Some("https://evil.example")).await.status, 403);
    assert!(inlet.heard().is_empty()); // nothing was heard
    assert_eq!(s.get(path).await.status, 200); // positive control
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wrong_scheme_an_unpaired_phone_and_no_link_door_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let s = serve(tmp.path(), Some(FakeInlet::new(Some(store_at(tmp.path())))), LinkChatOptions::default()).await;
    let mut conn = TcpStream::connect(("127.0.0.1", s.port)).await.unwrap();
    conn.write_all(format!("GET {} HTTP/1.1\r\nAuthorization: Basic {}\r\n\r\n", link_chat::MESSAGES_PATH, s.token).as_bytes())
        .await
        .unwrap();
    let mut raw = Vec::new();
    conn.read_to_end(&mut raw).await.unwrap();
    assert!(raw.starts_with(b"HTTP/1.1 401"));
    *s.check.lock() = None; // never paired
    assert_eq!(s.get(link_chat::MESSAGES_PATH).await.status, 401);
    let door = s.link.lock().take();
    assert_eq!(s.get(link_chat::MESSAGES_PATH).await.status, 404);
    *s.link.lock() = door;
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_chat_running_is_a_503_with_a_reason() {
    let tmp = tempfile::tempdir().unwrap();
    let inlet = FakeInlet::new(Some(store_at(tmp.path())));
    inlet.off.store(true, Ordering::SeqCst);
    let s = serve(tmp.path(), Some(Arc::clone(&inlet)), LinkChatOptions::default()).await;
    for (method, path, body) in [
        ("GET", link_chat::MESSAGES_PATH, Body::None),
        ("POST", link_chat::TURN_PATH, Body::Json(json!({"text": "hi", "client_id": "a"}))),
    ] {
        let got = s.req(method, path, body).await;
        assert!(got.status == 503 && got.headers["content-type"] == "application/json");
        assert_eq!(got.json(), json!({"ok": false, "error": link_chat::NO_CHAT}));
    }
    assert!(inlet.heard().is_empty());
    inlet.off.store(false, Ordering::SeqCst);
    assert_eq!(s.get(link_chat::MESSAGES_PATH).await.status, 200); // positive control
    s.close().await;
    let none = serve(&tmp.path().join("b"), None, LinkChatOptions::default()).await;
    assert_eq!(none.get(link_chat::MESSAGES_PATH).await.status, 503);
    none.close().await;
}

// ---- a turn ----------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_is_heard_once_as_typed_and_streams_each_line_then_done() {
    let tmp = tempfile::tempdir().unwrap();
    let inlet = FakeInlet::with(Some(store_at(tmp.path())), two_lines());
    let s = serve(tmp.path(), Some(Arc::clone(&inlet)), fast(0.01)).await;
    let got = s.turn("  hello eve  ", "c-1").await;
    assert!(got.status == 200 && got.headers["content-type"] == "text/event-stream");
    assert!(got.headers["transfer-encoding"] == "chunked" && got.headers["cache-control"] == "no-store");
    let (contents, done, _) = events(&got.body);
    assert!(contents == strs(&["First line.", "\n\nSecond line."]) && done); // a blank line between lines
    let heard = inlet.heard();
    assert_eq!(heard.len(), 1);
    let (text, on_call, app) = &heard[0];
    assert!(text == "hello eve" && !on_call && app.as_deref().is_some_and(|a| !a.is_empty())); // trimmed, typed
    assert_eq!(*inlet.delivered.lock(), vec![true, true]); // a reader had both
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_same_client_id_is_not_heard_again_and_gets_the_first_reply() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = FakeClock::new(1000.0);
    let inlet = FakeInlet::with(Some(store_at(tmp.path())), two_lines());
    let opts = LinkChatOptions { settle_secs: 0.01, clock: Some(clock.mono()), ..LinkChatOptions::default() };
    let s = serve(tmp.path(), Some(Arc::clone(&inlet)), opts).await;
    let first = events(&s.turn("hello", "same").await.body);
    let again = events(&s.turn("hello", "same").await.body);
    assert!(inlet.heard().len() == 1 && again == first && !first.0.is_empty()); // the first turn's reply, once heard
    let (a, b) = tokio::join!(s.turn("hi", "live"), s.turn("hi", "live"));
    assert_eq!(inlet.heard().len(), 2); // a retry while the first streams
    let want = (strs(&["First line.", "\n\nSecond line."]), true, 0);
    assert!(events(&a.body) == want && events(&b.body) == want);
    let other = events(&s.turn("hello", "other").await.body);
    assert!(inlet.heard().len() == 3 && other.1); // another id is another turn
    clock.add(link_chat::DEDUPE_SECS + 1.0);
    s.turn("hello", "same").await;
    assert_eq!(inlet.heard().len(), 4); // ten minutes on, it is new
    s.turn("hello", "").await;
    s.turn("hello", "").await;
    assert_eq!(inlet.heard().len(), 6); // no client_id: no dedupe
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_that_says_nothing_is_an_empty_completion() {
    let tmp = tempfile::tempdir().unwrap();
    let inlet = FakeInlet::new(Some(store_at(tmp.path()))); // hears, starts nothing
    let s = serve(tmp.path(), Some(inlet), fast(0.01)).await;
    let got = s.turn("hello", "c").await;
    assert!(got.status == 200 && events(&got.body) == (vec![], true, 0));
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_that_cannot_start_ends_at_once() {
    let tmp = tempfile::tempdir().unwrap();
    let inlet = FakeInlet::with(Some(store_at(tmp.path())), forever());
    inlet.hear_fails.store(true, Ordering::SeqCst);
    let s = serve(tmp.path(), Some(inlet), fast(0.01)).await;
    let t0 = Instant::now();
    let got = s.turn("hello", "c").await;
    assert!(got.status == 200 && events(&got.body) == (vec![], true, 0) && t0.elapsed() < Duration::from_secs(5));
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_line_said_while_the_words_are_heard_goes_to_the_app() {
    // the request is the turn's reader before hear runs: a quick answer said inside hear is never sent to Telegram
    let tmp = tempfile::tempdir().unwrap();
    let inlet = FakeInlet::new(Some(store_at(tmp.path())));
    *inlet.said_in_hear.lock() = Some("84.".to_string());
    let s = serve(tmp.path(), Some(Arc::clone(&inlet)), fast(0.01)).await;
    let got = events(&s.turn("what is 12 times 7", "c").await.body);
    assert_eq!(got, (strs(&["84."]), true, 0));
    assert_eq!(*inlet.delivered.lock(), vec![true]);
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_quiet_stream_is_kept_alive_and_ends_at_its_limit() {
    let tmp = tempfile::tempdir().unwrap();
    let inlet = FakeInlet::with(Some(store_at(tmp.path())), forever()); // busy and never done
    let opts = LinkChatOptions { settle_secs: 0.01, ping_secs: 0.05, turn_secs: 0.4, ..LinkChatOptions::default() };
    let s = serve(tmp.path(), Some(Arc::clone(&inlet)), opts).await;
    let t0 = Instant::now();
    let (contents, done, pings) = events(&s.turn("hello", "c").await.body);
    assert!(contents == strs(&["Working on it."]) && done && pings >= 3, "{pings}");
    let took = t0.elapsed();
    assert!(took >= Duration::from_millis(350) && took < Duration::from_secs(5));
    let out = inlet.app_out.lock().clone().unwrap();
    assert!(!out("nope", "late", "say")); // an unknown turn: to Telegram
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_that_hangs_up_sends_the_rest_to_telegram() {
    let tmp = tempfile::tempdir().unwrap();
    let gate = Arc::new(tokio::sync::Notify::new());
    let g = Arc::clone(&gate);
    let inlet = FakeInlet::with(
        Some(store_at(tmp.path())),
        script(move |inlet: Arc<FakeInlet>, app: String| {
            let g = Arc::clone(&g);
            async move {
                inlet.out(&app, "Read.", "say");
                g.notified().await;
                inlet.out(&app, "Nobody reads this.", "say");
                inlet.busy.store(false, Ordering::SeqCst);
            }
        }),
    );
    let s = serve(tmp.path(), Some(Arc::clone(&inlet)), fast(0.01)).await;
    let mut conn = TcpStream::connect(("127.0.0.1", s.port)).await.unwrap();
    let body = serde_json::to_vec(&json!({"text": "hi", "client_id": "gone-1234"})).unwrap();
    let head = format!(
        "POST {} HTTP/1.1\r\nAuthorization: Bearer {}\r\nContent-Length: {}\r\n\r\n",
        link_chat::TURN_PATH,
        s.token,
        body.len()
    );
    conn.write_all(head.as_bytes()).await.unwrap();
    conn.write_all(&body).await.unwrap();
    let mut buf = vec![0u8; 4096];
    let mut seen = Vec::new();
    while !String::from_utf8_lossy(&seen).contains("Read.") {
        let n = tokio::time::timeout(Duration::from_secs(5), conn.read(&mut buf)).await.unwrap().unwrap();
        assert!(n > 0);
        seen.extend_from_slice(&buf[..n]);
    }
    drop(conn); // the phone locked
    tokio::time::sleep(Duration::from_millis(200)).await;
    gate.notify_one();
    for _ in 0..200 {
        if inlet.delivered.lock().len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(*inlet.delivered.lock(), vec![true, false]); // the line after the hang-up goes to Telegram
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_limits_on_a_turn() {
    let cases: Vec<(Body, u16)> = vec![
        (Body::Json(json!({"text": ""})), 400),
        (Body::Json(json!({"text": "   "})), 400),
        (Body::Json(json!({"text": "x".repeat(4001)})), 400),
        (Body::Json(json!({"text": 5})), 400),
        (Body::Json(json!({"text": "hi", "client_id": "c".repeat(65)})), 400),
        (Body::Json(json!({"text": "hi", "client_id": 7})), 400),
        (Body::Json(json!(["hi"])), 400),
        (Body::Raw(b"not json".to_vec()), 400),
        (Body::Raw(Vec::new()), 400),
        (Body::Json(json!({"text": "x".repeat(4000), "client_id": "c".repeat(64)})), 200),
        (Body::Json(json!({"text": "hi"})), 200),
        (Body::Json(json!({"text": "hi", "client_id": null})), 200),
        (Body::Json(json!({"text": "hi", "grade": "fast"})), 400),
        (Body::Json(json!({"text": "hi", "grade": null})), 400),
        (Body::Json(json!({"text": "hi", "grade": 1})), 400),
        (Body::Json(json!({"text": "hi", "grade": "Think"})), 400),
        (Body::Json(json!({"text": "hi", "grade": "think"})), 200),
        (Body::Json(json!({"text": "hi", "grade": "instant"})), 200),
        (Body::Json(json!({"text": "hi", "grade": "auto"})), 200),
        (Body::Json(json!({"text": "x".repeat(70_000)})), 413),
    ];
    for (n, (body, status)) in cases.into_iter().enumerate() {
        let tmp = tempfile::tempdir().unwrap();
        let inlet = FakeInlet::new(Some(store_at(tmp.path())));
        let s = serve(tmp.path(), Some(Arc::clone(&inlet)), fast(0.01)).await;
        assert_eq!(s.req("POST", link_chat::TURN_PATH, body).await.status, status, "case {n}");
        assert_eq!(inlet.heard().len(), usize::from(status == 200), "case {n}");
        s.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wrong_methods_and_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let s = serve(tmp.path(), Some(FakeInlet::new(Some(store_at(tmp.path())))), LinkChatOptions::default()).await;
    assert_eq!(s.get(link_chat::TURN_PATH).await.status, 405);
    assert_eq!(s.post(link_chat::MESSAGES_PATH, json!({"x": 1})).await.status, 405);
    assert_eq!(s.get("/api/link/other").await.status, 404);
    assert_eq!(s.get(&format!("{}?after=bogus", link_chat::MESSAGES_PATH)).await.status, 400);
    assert_eq!(s.get(&format!("{}?wait=soon", link_chat::MESSAGES_PATH)).await.status, 400);
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_grade_reaches_the_inlet_and_auto_is_the_default() {
    let tmp = tempfile::tempdir().unwrap();
    let inlet = FakeInlet::new(Some(store_at(tmp.path())));
    let s = serve(tmp.path(), Some(Arc::clone(&inlet)), fast(0.01)).await;
    s.turn("hi", "a").await;
    for grade in ["instant", "think", "auto"] {
        let body = json!({"text": "hi", "client_id": grade, "grade": grade});
        assert_eq!(s.post(link_chat::TURN_PATH, body).await.status, 200);
    }
    assert_eq!(*inlet.grades.lock(), strs(&["auto", "instant", "think", "auto"]));
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_question_ends_the_stream_and_the_next_text_from_the_app_carries_the_rest() {
    let tmp = tempfile::tempdir().unwrap();
    let first_app = Arc::new(Mutex::new(String::new()));
    let fa = Arc::clone(&first_app);
    let inlet = FakeInlet::with(
        Some(store_at(tmp.path())),
        script(move |inlet: Arc<FakeInlet>, app: String| {
            let fa = Arc::clone(&fa);
            async move {
                let asked = { fa.lock().clone() };
                if asked.is_empty() {
                    *fa.lock() = app.clone();
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    inlet.out(&app, "Should I go ahead: book the table? Reply \"yes\" or \"no\".", "ask");
                    inlet.waiting.store(true, Ordering::SeqCst);
                } else {
                    // the answer: the first turn's reply goes on, said for the first turn's id
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    inlet.waiting.store(false, Ordering::SeqCst);
                    inlet.out(&asked, "Booked. You said yes.", "say");
                    inlet.busy.store(false, Ordering::SeqCst);
                }
            }
        }),
    );
    let s = serve(tmp.path(), Some(Arc::clone(&inlet)), fast(0.01)).await;
    let first = events(&s.turn("book a table", "c-1").await.body);
    assert!(first.1 && first.0.len() == 1 && first.0[0].contains("book the table?")); // it ends on the question
    assert_eq!(s.messages("").await["busy"], json!(false)); // waiting on the owner: no dots
    let second = events(&s.turn("yes", "c-2").await.body);
    assert_eq!(second, (strs(&["Booked. You said yes."]), true, 0)); // the rest of the reply, here
    assert_eq!(*inlet.delivered.lock(), vec![true, true]); // no Telegram copy of any of it
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn closing_the_server_ends_an_open_stream_with_done() {
    let tmp = tempfile::tempdir().unwrap();
    let inlet = FakeInlet::with(Some(store_at(tmp.path())), forever()); // busy and never done
    let s = Arc::new(serve(tmp.path(), Some(Arc::clone(&inlet)), fast(0.01)).await);
    let s2 = Arc::clone(&s);
    let pending = tokio::spawn(async move { s2.turn("hello", "c").await });
    for _ in 0..200 {
        if !inlet.delivered.lock().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    s.close().await;
    let got = pending.await.unwrap(); // dechunk checks the 0 chunk
    assert!(got.status == 200 && events(&got.body) == (strs(&["Working on it."]), true, 0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_app_turns_line_nobody_reads_goes_to_telegram() {
    let tmp = tempfile::tempdir().unwrap();
    let inlet = FakeInlet::new(Some(store_at(tmp.path())));
    let door = LinkChat::new(Arc::new(move || Some(Arc::clone(&inlet) as Arc<dyn Inlet>)), opts_at(tmp.path(), LinkChatOptions::default()));
    assert!(!door.deliver("a-turn-the-link-never-opened", "Nobody is reading.", "say")); // never lost
    assert!(!(door.app_out())("a-turn-the-link-never-opened", "Nor this.", "say"));
}

// ---- the conversation, read back ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_long_poll_wakes_on_a_new_line_and_times_out_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store_at(tmp.path());
    store.append("telegram", "t", "owner", "say", "first");
    let inlet = FakeInlet::new(Some(Arc::clone(&store)));
    let opts = LinkChatOptions { poll_secs: 30.0, ..LinkChatOptions::default() }; // a wake, never the re-read
    let s = serve(tmp.path(), Some(inlet), opts).await;
    let start = s.messages("").await;
    let cursor = start["cursor"].as_str().unwrap().to_string();
    assert!(start["messages"][0]["text"] == "first" && start["messages"].as_array().unwrap().len() == 1);
    assert_eq!(start["busy"], json!(false));
    let st = Arc::clone(&store);
    let later = async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        st.append("telegram", "t", "eve", "say", "second");
    };
    let t0 = Instant::now();
    let (got, _) = tokio::join!(s.messages(format!("?after={cursor}&wait=20")), later);
    let took = t0.elapsed();
    assert!(got["messages"].as_array().unwrap().len() == 1 && got["messages"][0]["text"] == "second");
    assert!(took >= Duration::from_millis(250) && took < Duration::from_secs(5), "{took:?}");
    let next = got["cursor"].as_str().unwrap().to_string();
    let t0 = Instant::now();
    let empty = s.messages(&format!("?after={next}&wait=0.4")).await;
    assert_eq!(empty, json!({"messages": [], "cursor": next, "busy": false}));
    let took = t0.elapsed();
    assert!(took >= Duration::from_millis(350) && took < Duration::from_secs(5));
    assert_eq!(s.messages(&format!("?after={next}")).await["messages"], json!([])); // wait=0: at once
    // a second poll and a third: still one wake-up on the store (every append wakes, none is missed)
    let st = Arc::clone(&store);
    let later = async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        st.append("telegram", "t", "eve", "say", "third");
    };
    let (got, _) = tokio::join!(s.messages(format!("?after={next}&wait=20")), later);
    assert_eq!(got["messages"][0]["text"], "third");
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_long_poll_also_sees_a_write_from_another_process() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store_at(tmp.path());
    store.append("telegram", "t", "owner", "say", "first");
    let opts = LinkChatOptions { poll_secs: 0.1, ..LinkChatOptions::default() };
    let s = serve(tmp.path(), Some(FakeInlet::new(Some(store))), opts).await;
    let cursor = s.messages("").await["cursor"].as_str().unwrap().to_string();
    let other = store_at(tmp.path()); // no listener
    let later = async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        other.append("telegram", "t", "eve", "say", "from elsewhere");
    };
    let (got, _) = tokio::join!(s.messages(format!("?after={cursor}&wait=10")), later);
    assert_eq!(got["messages"][0]["text"], "from elsewhere");
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn busy_is_a_turn_in_flight_and_memory_off_is_a_503() {
    let tmp = tempfile::tempdir().unwrap();
    let inlet = FakeInlet::new(Some(store_at(tmp.path())));
    let s = serve(tmp.path(), Some(Arc::clone(&inlet)), LinkChatOptions::default()).await;
    assert_eq!(s.messages("").await["busy"], json!(false));
    inlet.busy.store(true, Ordering::SeqCst);
    assert_eq!(s.messages("").await["busy"], json!(true));
    inlet.waiting.store(true, Ordering::SeqCst);
    assert_eq!(s.messages("").await["busy"], json!(false)); // only waiting on the owner's answer: no dots
    *inlet.transcripts.lock() = None;
    let got = s.get(link_chat::MESSAGES_PATH).await;
    assert!(got.status == 503 && got.json()["error"] == link_chat::NO_MEMORY);
    let off = Transcripts::new(TranscriptConfig::new(false, tmp.path().join("off")), None, None);
    *inlet.transcripts.lock() = Some(Arc::new(off));
    assert_eq!(s.get(link_chat::MESSAGES_PATH).await.status, 503);
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_long_poll_returns_when_busy_clears_not_at_its_timeout() {
    let tmp = tempfile::tempdir().unwrap();
    let inlet = FakeInlet::new(Some(store_at(tmp.path())));
    inlet.busy.store(true, Ordering::SeqCst);
    let opts = LinkChatOptions { poll_secs: 0.05, ..LinkChatOptions::default() };
    let s = serve(tmp.path(), Some(Arc::clone(&inlet)), opts).await;
    let cursor = s.messages("").await["cursor"].as_str().unwrap().to_string();
    let i = Arc::clone(&inlet);
    let clear = async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        i.busy.store(false, Ordering::SeqCst);
    };
    let t0 = Instant::now();
    let (got, _) = tokio::join!(s.messages(format!("?after={cursor}&wait=20")), clear);
    assert!(got["messages"] == json!([]) && got["busy"] == json!(false) && t0.elapsed() < Duration::from_secs(5));
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_older_route_answers_a_page_and_refuses_a_bad_id() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store_at(tmp.path());
    let day = NaiveDate::from_ymd_opt(2026, 10, 13).unwrap();
    for k in 0..5 {
        let now = eve_transcripts::py::local_aware(NaiveDateTime::new(day, NaiveTime::from_hms_opt(9, k, 0).unwrap()));
        store.append_with("telegram", "t-1", "owner", "say", &format!("line {k}"), Append { now: Some(now), ..Append::default() });
    }
    let s = serve(tmp.path(), Some(FakeInlet::new(Some(store))), LinkChatOptions::default()).await;
    let got = s.get(&format!("{}?before=2026-10-13:3", link_chat::MESSAGES_PATH)).await;
    let page = got.json();
    let texts: Vec<&str> = page["messages"].as_array().unwrap().iter().map(|m| m["text"].as_str().unwrap()).collect();
    assert!(got.status == 200 && texts == ["line 0", "line 1", "line 2"]);
    assert!(page["more"] == json!(false) && page["cursor"] == "");
    for bad in ["x", "2026-10-13:-1", "../a"] {
        assert_eq!(s.get(&format!("{}?before={bad}", link_chat::MESSAGES_PATH)).await.status, 400, "{bad}");
    }
    s.close().await;
}

// ---- reactions ------------------------------------------------------------------------------------------

const CID: &str = "c-0123456789";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reaction_wakes_the_long_poll_and_an_old_version_answers_at_once() {
    let tmp = tempfile::tempdir().unwrap();
    let inlet = FakeInlet::new(Some(store_at(tmp.path())));
    let reactions = Arc::new(Reactions::new(None));
    let mid = reactions.mint(CID).unwrap();
    *inlet.reactions.lock() = Some(Arc::clone(&reactions));
    let s = serve(tmp.path(), Some(inlet), LinkChatOptions::default()).await;
    let first = s.messages("").await;
    assert!(first["reactions"] == json!({}) && first["cursor"].as_str().unwrap().contains('~'));
    let r = Arc::clone(&reactions);
    let later = async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        r.set(mid, "👀").unwrap();
    };
    let t0 = Instant::now();
    let (got, _) = tokio::join!(s.messages(format!("?after={}&wait=20", first["cursor"].as_str().unwrap())), later);
    assert!(t0.elapsed() < Duration::from_secs(10)); // woken, not timed out
    assert!(got["messages"] == json!([]) && got["reactions"] == json!({CID: "👀"}));
    // the app's cursor names the version it saw: a change it missed between polls answers at once
    reactions.set(mid, "👍").unwrap();
    let t0 = Instant::now();
    let again = s.messages(&format!("?after={}&wait=20", got["cursor"].as_str().unwrap())).await;
    assert!(t0.elapsed() < Duration::from_secs(5) && again["reactions"] == json!({CID: "👍"}));
    // nothing changed since: the poll waits as before
    let cursor = again["cursor"].as_str().unwrap().to_string();
    let quiet = s.messages(&format!("?after={cursor}&wait=0.4")).await;
    assert!(quiet["messages"] == json!([]) && quiet["cursor"] == json!(cursor));
    // a cursor this door never made is refused
    let base = cursor.split('~').next().unwrap();
    for bad in [format!("{cursor}~"), format!("{base}~x"), "2026-10-09:1~1~2".to_string()] {
        assert_eq!(s.get(&format!("{}?after={bad}", link_chat::MESSAGES_PATH)).await.status, 400, "{bad}");
    }
    s.close().await;
}

// ---- the widget's feed (link_today) ----------------------------------------------------------------------

fn local(y: i32, m: u32, d: u32, h: u32, min: u32) -> eve_transcripts::Dt {
    let day = NaiveDate::from_ymd_opt(y, m, d).unwrap();
    eve_transcripts::py::local_aware(NaiveDateTime::new(day, NaiveTime::from_hms_opt(h, min, 0).unwrap()))
}

fn today_now() -> eve_transcripts::Dt {
    local(2026, 10, 8, 14, 0) // 2 pm local
}

fn at(hour: u32, minute: u32, days: u32) -> String {
    local(2026, 10, 8 + days, hour, minute).to_rfc3339()
}

fn ev(summary: &str, start: String, end: String, extra: Value) -> Value {
    let mut out = json!({"id": summary, "summary": summary, "start": {"dateTime": start}, "end": {"dateTime": end}});
    if let (Value::Object(o), Value::Object(e)) = (&mut out, extra) {
        o.extend(e);
    }
    out
}

fn calendar_events() -> Vec<Value> {
    vec![
        ev("Breakfast", at(8, 0, 0), at(9, 0, 0), json!({})), // over
        ev("Design review", at(13, 30, 0), at(14, 30, 0), json!({"location": "Room 4"})), // on now
        ev("Dentist", at(16, 0, 0), at(17, 0, 0), json!({})),
        ev("Declined sync", at(17, 0, 0), at(18, 0, 0), json!({"attendees": [{"self": true, "responseStatus": "declined"}]})),
        ev("Cancelled call", at(18, 0, 0), at(19, 0, 0), json!({"status": "cancelled"})),
        ev("Tomorrow's standup", at(9, 0, 1), at(10, 0, 1), json!({})), // not today
        json!({"id": "trip", "summary": "Trip", "start": {"date": "2026-10-08"}, "end": {"date": "2026-10-09"}}),
    ]
}

/// apps_mcp.AppsBridge as link_today uses it: `apps` and `execute`.
struct FakeApps {
    apps: Vec<String>,
    events: Vec<Value>,
    fail: bool,
    delay: Mutex<f64>,
    calls: Mutex<Vec<(String, Obj)>>,
}

impl FakeApps {
    fn new(apps: &[&str], fail: bool, delay: f64) -> Arc<Self> {
        Arc::new(FakeApps {
            apps: strs(apps),
            events: calendar_events(),
            fail,
            delay: Mutex::new(delay),
            calls: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl DirectApps for FakeApps {
    fn apps(&self) -> Vec<String> {
        self.apps.clone()
    }

    async fn execute(&self, name: &str, args: &Obj) -> Obj {
        self.calls.lock().push((name.to_string(), args.clone()));
        let delay = *self.delay.lock();
        if delay > 0.0 {
            tokio::time::sleep(Duration::from_secs_f64(delay)).await;
        }
        let out = if self.fail {
            json!({"ok": false, "reason": "GoogleError: 401"})
        } else {
            json!({"ok": true, "result": {"timeZone": "America/Los_Angeles", "events": self.events}})
        };
        out.as_object().unwrap().clone()
    }
}

/// A card's `panel(body)`: records each body, answers a fixed dict; `listing` is the lights'.
struct FakePanel {
    answer: Value,
    bodies: Mutex<Vec<Value>>,
    listing: Vec<Value>,
    available: AtomicBool,
    delay: f64,
    broken: bool,
    picture: Option<Vec<u8>>,
}

impl FakePanel {
    fn new(answer: Value) -> Arc<Self> {
        Self::build(answer, Vec::new(), 0.0, false, None)
    }

    fn build(answer: Value, listing: Vec<Value>, delay: f64, broken: bool, picture: Option<Vec<u8>>) -> Arc<Self> {
        Arc::new(FakePanel {
            answer,
            bodies: Mutex::new(Vec::new()),
            listing,
            available: AtomicBool::new(true),
            delay,
            broken,
            picture,
        })
    }

    fn bodies(&self) -> Vec<Value> {
        self.bodies.lock().clone()
    }
}

#[async_trait]
impl Hub for FakePanel {
    fn available(&self) -> bool {
        self.available.load(Ordering::SeqCst)
    }

    fn listing(&self) -> Vec<Value> {
        self.listing.clone()
    }

    async fn panel(&self, body: &Value) -> anyhow::Result<Value> {
        self.bodies.lock().push(body.clone());
        if self.delay > 0.0 {
            tokio::time::sleep(Duration::from_secs_f64(self.delay)).await;
        }
        if self.broken {
            anyhow::bail!("the bulb said no");
        }
        Ok(self.answer.clone())
    }

    async fn stream(&self, writer: &mut DynWrite) -> std::io::Result<()> {
        let Some(picture) = &self.picture else {
            return Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "this card has no picture"));
        };
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: multipart/x-mixed-replace; boundary=frame\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            picture.len()
        );
        writer.write_all(head.as_bytes()).await?;
        writer.write_all(picture).await?;
        writer.flush().await
    }
}

fn watches() -> Vec<Watch> {
    let mut w1 = Watch::new("w1", "price", "https://shop.example/chair", "Desk chair", "below");
    w1.value = Some(300.0);
    w1.currency = "USD".into();
    w1.checks = 3;
    w1.last_value = Some(320.0);
    let mut w2 = Watch::new("w2", "price", "https://shop.example/lamp", "Lamp", "below");
    w2.value = Some(40.0);
    w2.paused = true;
    let mut w3 = Watch::new("w3", "quote", "AAPL", "AAPL", "below");
    w3.value = Some(200.0);
    w3.errors = eve_web::watch::TELL_AFTER_ERRORS;
    w3.checks = 9;
    let w4 = Watch::new("w4", "price", "https://shop.example/desk", "Desk", "change");
    vec![w1, w2, w3, w4]
}

fn today_door(source_secs: f64, calendar_secs: f64, mono: Option<eve_link::Mono>) -> Arc<Today> {
    Arc::new(Today::new(TodayOptions {
        now: Some(Arc::new(today_now)),
        mono,
        source_secs,
        calendar_secs,
        ..TodayOptions::default()
    }))
}

fn with_today(today: Arc<Today>) -> LinkChatOptions {
    LinkChatOptions { today: Some(today), ..LinkChatOptions::default() }
}

async fn today(s: &Server) -> Value {
    let got = s.get(link_chat::TODAY_PATH).await;
    assert_eq!(got.status, 200, "{}", String::from_utf8_lossy(&got.body));
    assert_eq!(got.headers.get("cache-control").map(String::as_str), Some("no-store"));
    got.json()
}

fn ts(at: &eve_transcripts::Dt) -> f64 {
    (at.timestamp() * 1_000_000 + i64::from(at.timestamp_subsec_micros())) as f64 / 1e6
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn today_needs_the_link_token_no_origin_and_a_get() {
    let tmp = tempfile::tempdir().unwrap();
    let inlet = FakeInlet::new(Some(store_at(tmp.path())));
    let s = serve(tmp.path(), Some(inlet), with_today(today_door(4.0, 8.0, None))).await;
    let path = link_chat::TODAY_PATH;
    assert_eq!(http(s.port, "GET", path, Body::None, None, None).await.status, 401); // no token
    assert_eq!(http(s.port, "GET", path, Body::None, Some(&s.token[..s.token.len() - 1]), None).await.status, 401);
    assert_eq!(http(s.port, "GET", path, Body::None, Some(&s.token), Some("https://evil.example")).await.status, 403);
    assert_eq!(http(s.port, "GET", path, Body::None, Some(&s.token), Some("null")).await.status, 403);
    assert_eq!(s.post(path, json!({})).await.status, 405);
    assert_eq!(s.get(&format!("{path}/x")).await.status, 404);
    assert_eq!(s.get(path).await.status, 200); // positive control
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn today_gathers_jobs_watches_calendar_and_what_plays() {
    let tmp = tempfile::tempdir().unwrap();
    let chief = FakeChief::three();
    let apps = FakeApps::new(&["calendar"], false, 0.0);
    let inlet = FakeInlet::new(Some(store_at(tmp.path())));
    inlet.set_chief(Some(chief.clone()));
    *inlet.watches.lock() = Some(watches());
    *inlet.direct.lock() = Some(apps.clone());
    let s = serve(tmp.path(), Some(Arc::clone(&inlet)), with_today(today_door(4.0, 8.0, None))).await;
    let spotify = FakePanel::new(json!({"ok": true, "playing": true, "track": "A song", "artists": ["Someone", "Else"]}));
    let tv = FakePanel::new(json!({"ok": true, "power": "on", "app": "YouTube", "volume": 40, "line": "TV on"}));
    s.mini.set("spotify", spotify.clone());
    s.mini.set("firetv", tv.clone());
    let got = today(&s).await;
    assert_eq!(got["sources"], json!({"jobs": "ok", "watches": "ok", "calendar": "ok", "music": "ok", "tv": "ok"}));
    assert_eq!(got["at"].as_f64().unwrap(), ts(&today_now()));
    // the chief: three cards waiting on their Go, all needing the owner, none merely going
    let ids: Vec<&str> = got["needs"].as_array().unwrap().iter().map(|n| n["id"].as_str().unwrap()).collect();
    assert!(got["needs_you"] == 3 && ids == ["c1", "c2", "c3"]);
    assert!(got["needs"].as_array().unwrap().iter().all(|n| n["kind"] == "job"
        && n["line"] == "Waiting on your Go"
        && !n["title"].as_str().unwrap().is_empty()));
    // the watches that are on, after the chief's going cards (none here); the paused lamp is left out
    let going: Vec<(&str, &str)> =
        got["going"].as_array().unwrap().iter().map(|g| (g["id"].as_str().unwrap(), g["kind"].as_str().unwrap())).collect();
    assert_eq!(going, [("w1", "watch"), ("w3", "watch"), ("w4", "watch")]);
    assert_eq!(got["going"][0]["title"], "Desk chair");
    assert_eq!(got["going"][0]["line"], json!(eve_web::watch::rules::now_line(&watches()[0])));
    assert!(got["going"][1]["line"] == "can't read it" && got["going"][2]["line"] == "no reading yet");
    // the calendar: the all-day trip first, then what has not ended, soonest first; nothing tomorrow, declined or
    // cancelled
    let titles: Vec<&str> = got["today"].as_array().unwrap().iter().map(|e| e["title"].as_str().unwrap()).collect();
    assert_eq!(titles, ["Trip", "Design review", "Dentist"]);
    let (trip, review, dentist) = (&got["today"][0], &got["today"][1], &got["today"][2]);
    assert!(trip["all_day"] == true && review["all_day"] == false && review["place"] == "Room 4");
    assert_eq!(review["start"].as_f64().unwrap(), ts(&local(2026, 10, 8, 13, 30)));
    assert_eq!(dentist["end"].as_f64().unwrap(), ts(&local(2026, 10, 8, 17, 0)));
    let (name, args) = apps.calls.lock()[0].clone();
    assert_eq!(name, "calendar__list_events");
    let start = chrono::DateTime::parse_from_rfc3339(args["startTime"].as_str().unwrap()).unwrap();
    let end = chrono::DateTime::parse_from_rfc3339(args["endTime"].as_str().unwrap()).unwrap();
    assert!(start == today_now() && end == local(2026, 10, 9, 0, 0));
    assert!(args["orderBy"] == "startTime" && args["pageSize"] == 25);
    // what plays: Spotify's track and the TV's app, read with the cards' own status action
    assert_eq!(
        got["now"],
        json!([{"kind": "music", "title": "A song", "line": "Someone, Else"}, {"kind": "tv", "title": "TV", "line": "YouTube"}])
    );
    assert!(spotify.bodies() == vec![json!({"action": "status"})] && tv.bodies() == vec![json!({"action": "status"})]);
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn open_cards_that_do_not_need_the_owner_are_going() {
    let tmp = tempfile::tempdir().unwrap();
    let chief = FakeChief::three();
    let inlet = FakeInlet::new(Some(store_at(tmp.path())));
    inlet.set_chief(Some(chief.clone()));
    assert!(chief.approve("c2", "id", None).await.unwrap()["ok"] == true); // approved, not yet run: going
    chief.drop_job("c3").await.unwrap(); // finished: on neither list
    let s = serve(tmp.path(), Some(inlet), with_today(today_door(4.0, 8.0, None))).await;
    let got = today(&s).await;
    assert!(got["needs_you"] == 1 && got["needs"].as_array().unwrap().len() == 1 && got["needs"][0]["id"] == "c1");
    assert!(got["going"].as_array().unwrap().len() == 1 && got["going"][0]["id"] == "c2" && got["going"][0]["kind"] == "job");
    assert!(got["sources"]["watches"] == "off" && got["sources"]["calendar"] == "off");
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nothing_on_is_an_empty_answer_not_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let s = serve(tmp.path(), None, with_today(today_door(4.0, 8.0, None))).await; // no chat, no chief, no cards
    let got = today(&s).await;
    let off: Map<String, Value> = ["jobs", "watches", "calendar", "music", "tv"].iter().map(|k| (k.to_string(), json!("off"))).collect();
    assert_eq!(
        got,
        json!({"at": ts(&today_now()), "needs_you": 0, "needs": [], "going": [], "today": [], "now": [], "sources": off})
    );
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_calendar_that_is_not_signed_in_is_off() {
    let tmp = tempfile::tempdir().unwrap();
    let inlet = FakeInlet::new(Some(store_at(tmp.path())));
    let apps = FakeApps::new(&["gmail", "drive"], false, 0.0);
    *inlet.direct.lock() = Some(apps.clone());
    let s = serve(tmp.path(), Some(inlet), with_today(today_door(4.0, 8.0, None))).await;
    let got = today(&s).await;
    assert!(got["sources"]["calendar"] == "off" && apps.calls.lock().is_empty());
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_or_broken_source_is_left_out_and_the_rest_still_come() {
    let tmp = tempfile::tempdir().unwrap();
    let chief = FakeChief::three();
    let inlet = FakeInlet::new(Some(store_at(tmp.path())));
    inlet.set_chief(Some(chief.clone()));
    *inlet.direct.lock() = Some(FakeApps::new(&["calendar"], true, 0.0));
    let s = serve(tmp.path(), Some(Arc::clone(&inlet)), with_today(today_door(0.3, 0.3, None))).await;
    s.mini.set("spotify", FakePanel::build(json!({"ok": true, "playing": true, "track": "x"}), Vec::new(), 5.0, false, None));
    s.mini.set("firetv", FakePanel::build(json!({}), Vec::new(), 0.0, true, None));
    let started = Instant::now();
    let got = today(&s).await;
    assert!(started.elapsed() < Duration::from_secs(3)); // the slow card did not hold the answer
    assert_eq!(got["sources"], json!({"jobs": "ok", "watches": "off", "calendar": "failed", "music": "failed", "tv": "failed"}));
    assert!(got["needs_you"] == 3 && got["today"] == json!([]) && got["now"] == json!([]));
    inlet.set_chief(None);
    inlet.watches_break.store(true, Ordering::SeqCst);
    let got = today(&s).await;
    assert!(got["sources"]["jobs"] == "off" && got["sources"]["watches"] == "failed" && got["going"] == json!([]));
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_calendar_is_left_out_then_kept_for_the_next_refresh() {
    let tmp = tempfile::tempdir().unwrap();
    let apps = FakeApps::new(&["calendar"], false, 0.6);
    let mono = FakeClock::new(1000.0);
    let inlet = FakeInlet::new(Some(store_at(tmp.path())));
    *inlet.direct.lock() = Some(apps.clone());
    let s = serve(tmp.path(), Some(inlet), with_today(today_door(4.0, 0.2, Some(mono.mono())))).await;
    let got = today(&s).await;
    assert!(got["sources"]["calendar"] == "failed" && got["today"] == json!([]));
    tokio::time::sleep(Duration::from_millis(800)).await; // the read finished after the answer went
    let got = today(&s).await;
    assert!(got["sources"]["calendar"] == "ok" && got["today"].as_array().unwrap().len() == 3);
    assert_eq!(apps.calls.lock().len(), 1); // kept, not read again
    mono.add(301.0); // past CALENDAR_KEEP_SECS: read again
    *apps.delay.lock() = 0.0;
    today(&s).await;
    assert_eq!(apps.calls.lock().len(), 2);
    s.close().await;
}

#[test]
fn events_part_keeps_today_only_and_reads_both_shapes() {
    let mut all = calendar_events();
    all.push(json!({"summary": "no start"}));
    all.push(json!({"id": "bad", "summary": "Bad", "start": {"dateTime": "soon"}}));
    let raw = json!({"ok": true, "result": {"events": all}});
    let titles = |rows: Vec<Obj>| rows.iter().map(|e| e["title"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    assert_eq!(titles(events_part(&raw, today_now())), strs(&["Trip", "Design review", "Dentist"]));
    let late = local(2026, 10, 8, 23, 30);
    assert_eq!(titles(events_part(&raw, late)), strs(&["Trip"]));
    let mut untitled = ev("x", at(15, 0, 0), at(16, 0, 0), json!({}));
    untitled["summary"] = json!("");
    let got = events_part(&json!({"result": {"events": [untitled]}}), today_now());
    assert_eq!(titles(got), strs(&["Busy"])); // a private event still holds the time
    assert!(events_part(&Value::Null, today_now()).is_empty() && events_part(&json!("clipped text"), today_now()).is_empty());
}

#[test]
fn music_and_tv_rows_only_while_something_plays() {
    assert!(music_part(Some(&json!({"ok": true, "playing": false, "track": "x"}))).is_empty());
    assert!(music_part(Some(&json!({"ok": false, "reason": "Couldn't reach Spotify."}))).is_empty());
    assert!(music_part(Some(&json!({"ok": true, "playing": true, "track": null}))).is_empty());
    let long = music_part(Some(&json!({"ok": true, "playing": true, "track": "x".repeat(200), "artists": []})));
    let title = long[0]["title"].as_str().unwrap();
    assert!(title.ends_with('…') && title.chars().count() == eve_link::link_today::TITLE_CHARS);
    assert!(tv_part(Some(&json!({"ok": true, "power": "standby"}))).is_empty());
    assert!(tv_part(Some(&json!({"ok": false, "reachable": false}))).is_empty());
    let row = |v: Value| Value::Array(tv_part(Some(&v)).into_iter().map(Value::Object).collect());
    assert_eq!(row(json!({"ok": true, "power": "screensaver"})), json!([{"kind": "tv", "title": "TV", "line": "Screensaver"}]));
    assert_eq!(row(json!({"ok": true, "power": "on", "app": null})), json!([{"kind": "tv", "title": "TV", "line": "On"}]));
    assert!(music_part(None).is_empty() && tv_part(Some(&json!("on"))).is_empty());
}

#[test]
fn clip_is_one_line_with_an_ellipsis() {
    use eve_link::link_today::clip;
    assert_eq!(clip(&json!("  a\n b\tc  "), 80), "a b c");
    assert_eq!(clip(&Value::Null, 10), "");
    assert_eq!(clip(&json!(0), 10), "");
    assert_eq!(clip(&json!(12), 10), "12");
    assert_eq!(clip(&json!("abcdef ghij"), 8), "abcdef…"); // cut, trailing space dropped, then the ellipsis
}

// ---- the chief's cards: the Work view ------------------------------------------------------------------------

/// (verb, card id, via, rev) of one call on the chief.
type ChiefCall = (String, String, Option<String>, Option<i64>);

/// The chief as the door calls it: its cards (as views) and its own methods, each recorded.
struct FakeChief {
    cards: Mutex<Vec<JobCard>>,
    titles: Mutex<HashMap<String, String>>,
    calls: Mutex<Vec<ChiefCall>>,
    clock: Mutex<f64>,
    fail: AtomicBool,
}

fn phase(n: i64, status: &str, door: &str, do_: &str) -> JobPhase {
    JobPhase { n, status: status.into(), door: door.into(), approval: None, do_: do_.into() }
}

impl FakeChief {
    fn new() -> Arc<Self> {
        Arc::new(FakeChief {
            cards: Mutex::new(Vec::new()),
            titles: Mutex::new(HashMap::new()),
            calls: Mutex::new(Vec::new()),
            clock: Mutex::new(1_790_000_000.0),
            fail: AtomicBool::new(false),
        })
    }

    /// Three Standing desk cards, each waiting on its Go (test_chief's `take` three times).
    fn three() -> Arc<Self> {
        let chief = Self::new();
        for _ in 0..3 {
            chief.take_on("Standing desk");
        }
        chief
    }

    /// take_on: a new card waiting on its Go → its id.
    fn take_on(&self, title: &str) -> String {
        let mut cards = self.cards.lock();
        let id = format!("c{}", cards.len() + 1);
        let mut card = JobCard {
            id: id.clone(),
            kind: "task".into(),
            status: "waiting".into(),
            waiting_for: Some("go".into()),
            terminal: false,
            rev: 1,
            updated: *self.clock.lock(),
            phases: vec![phase(1, "ok", "two_way", "research"), phase(2, "ok", "two_way", "assess"), phase(3, "queued", "one_way", "act")],
            cue_at: None,
            doubtful: false,
            data: Map::new(),
        };
        self.titles.lock().insert(id.clone(), title.to_string());
        Self::sync(&mut card, title);
        cards.push(card);
        id
    }

    /// `Chief._as_data`: what the card says of itself.
    fn sync(card: &mut JobCard, title: &str) {
        let data = json!({"id": card.id, "kind": card.kind, "title": title, "status": card.status,
                          "waiting_for": card.waiting_for});
        card.data = data.as_object().unwrap().clone();
    }

    fn tick(&self, s: f64) {
        *self.clock.lock() += s;
    }

    fn change<T>(&self, cid: &str, f: impl FnOnce(&mut JobCard) -> T) -> Option<T> {
        let now = *self.clock.lock();
        let title = self.titles.lock().get(cid).cloned().unwrap_or_default();
        let mut cards = self.cards.lock();
        let card = cards.iter_mut().find(|c| c.id == cid)?;
        let out = f(card);
        card.updated = now;
        Self::sync(card, &title);
        Some(out)
    }

    fn record(&self, verb: &str, cid: &str, via: Option<&str>, rev: Option<i64>) -> anyhow::Result<()> {
        self.calls.lock().push((verb.into(), cid.into(), via.map(str::to_string), rev));
        if self.fail.load(Ordering::SeqCst) {
            anyhow::bail!("the ledger is locked");
        }
        Ok(())
    }

    fn status(&self, cid: &str) -> String {
        self.card(cid).map(|c| c.status).unwrap_or_default()
    }

    fn verbs(&self) -> Vec<String> {
        self.calls.lock().iter().map(|(v, c, _, _)| format!("{v} {c}")).collect()
    }
}

fn obj_of(v: Value) -> Obj {
    v.as_object().unwrap().clone()
}

#[async_trait]
impl Chief for FakeChief {
    fn cards(&self) -> Vec<JobCard> {
        self.cards.lock().clone()
    }

    fn card(&self, cid: &str) -> Option<JobCard> {
        self.cards.lock().iter().find(|c| c.id == cid).cloned()
    }

    async fn approve(&self, cid: &str, via: &str, rev: Option<i64>) -> anyhow::Result<Obj> {
        self.record("approve", cid, Some(via), rev)?;
        let Some(card) = self.card(cid) else { return Ok(obj_of(json!({"ok": false, "reason": "no such card"}))) };
        if rev.is_some_and(|r| r != card.rev) {
            return Ok(obj_of(json!({"ok": false, "reason": format!("{cid} changed since you saw it; look again")})));
        }
        self.change(cid, |c| {
            c.phases[2].approval = Some(json!({"via": via}));
            c.status = "running".into();
            c.waiting_for = None;
        });
        Ok(obj_of(json!({"ok": true, "id": cid, "phase": 3})))
    }

    async fn done(&self, cid: &str) -> anyhow::Result<Obj> {
        self.record("done", cid, None, None)?;
        self.change(cid, |c| {
            c.status = "done".into();
            c.terminal = true;
        });
        Ok(obj_of(json!({"ok": true, "id": cid})))
    }

    async fn drop_job(&self, cid: &str) -> anyhow::Result<Obj> {
        self.record("drop", cid, None, None)?;
        if self.card(cid).is_some_and(|c| c.terminal) {
            return Ok(obj_of(json!({"ok": false, "reason": format!("{cid} is already finished")})));
        }
        self.change(cid, |c| {
            c.status = "dropped".into();
            c.terminal = true;
        });
        Ok(obj_of(json!({"ok": true, "id": cid})))
    }

    async fn happened(&self, cid: &str) -> anyhow::Result<Obj> {
        self.record("happened", cid, None, None)?;
        if !self.card(cid).is_some_and(|c| c.doubtful) {
            return Ok(obj_of(json!({"ok": false, "reason": format!("{cid} has nothing in doubt")})));
        }
        self.change(cid, |c| {
            c.doubtful = false;
            c.phases[2].status = "ok".into();
        });
        Ok(obj_of(json!({"ok": true, "id": cid})))
    }

    async fn didnt(&self, cid: &str) -> anyhow::Result<Obj> {
        self.record("didnt", cid, None, None)?;
        if !self.card(cid).is_some_and(|c| c.doubtful) {
            return Ok(obj_of(json!({"ok": false, "reason": format!("{cid} has nothing in doubt")})));
        }
        self.change(cid, |c| {
            c.doubtful = false;
            c.status = "waiting".into();
            c.waiting_for = Some("go".into());
            c.phases[2].status = "queued".into();
        });
        Ok(obj_of(json!({"ok": true, "id": cid})))
    }

    async fn raise_budget(&self, cid: &str) -> anyhow::Result<Obj> {
        self.record("raise", cid, None, None)?;
        Ok(obj_of(json!({"ok": true, "id": cid, "usd": 10.0, "minutes": 60})))
    }
}

async fn jobs(s: &Server) -> Value {
    let got = s.get(link_chat::JOBS_PATH).await;
    assert_eq!(got.status, 200, "{}", String::from_utf8_lossy(&got.body));
    got.json()
}

async fn act(s: &Server, cid: &str, verb: &str, body: Body) -> (u16, Value) {
    let got = s.req("POST", &format!("{}/{cid}/{verb}", link_chat::JOBS_PATH), body).await;
    (got.status, got.json())
}

fn ids(list: &Value) -> Vec<String> {
    list.as_array().unwrap().iter().map(|j| j["id"].as_str().unwrap().to_string()).collect()
}

/// The owner's built apps (apps_maker.AppStore).
struct FakeStore(Vec<AppInfo>);

impl AppStoreView for FakeStore {
    fn list(&self) -> Vec<AppInfo> {
        self.0.clone()
    }

    fn info(&self, slug: &str) -> Option<AppInfo> {
        self.0.iter().find(|a| a.slug == slug).cloned()
    }
}

fn app_store() -> Arc<FakeStore> {
    Arc::new(FakeStore(vec![
        AppInfo { slug: "plant-log".into(), title: "Plant log".into(), updated: 1_790_000_100.0 },
        AppInfo { slug: "tip-jar".into(), title: "Tip jar".into(), updated: 1_790_000_000.0 },
    ]))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_new_path_needs_the_link_token_and_no_origin() {
    let tmp = tempfile::tempdir().unwrap();
    let chief = FakeChief::three();
    let inlet = FakeInlet::new(Some(store_at(tmp.path())));
    inlet.set_chief(Some(chief.clone()));
    let s = serve(tmp.path(), Some(inlet), LinkChatOptions::default()).await;
    *s.mini.store.lock() = Some(app_store());
    let doors = [
        ("GET", link_chat::JOBS_PATH.to_string()),
        ("POST", format!("{}/c1/drop", link_chat::JOBS_PATH)),
        ("GET", link_chat::APPS_PATH.to_string()),
        ("POST", format!("{}/tip-jar/open", link_chat::APPS_PATH)),
    ];
    let short = &s.token[..s.token.len() - 1];
    for (method, path) in &doors {
        assert_eq!(http(s.port, method, path, Body::None, None, None).await.status, 401, "{path}"); // no token
        assert_eq!(http(s.port, method, path, Body::None, Some(short), None).await.status, 401, "{path}");
        assert_eq!(http(s.port, method, path, Body::None, Some(&s.token), Some("https://evil.example")).await.status, 403);
        assert_eq!(http(s.port, method, path, Body::None, Some(&s.token), Some("null")).await.status, 403, "{path}");
    }
    assert_eq!(chief.status("c1"), "waiting"); // nothing was done
    for (method, path) in &doors {
        assert_eq!(s.req(method, path, Body::None).await.status, 200, "{path}"); // positive controls
    }
    assert_eq!(chief.status("c1"), "dropped");
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn jobs_list_open_cards_first_needs_you_first_then_the_finished() {
    let tmp = tempfile::tempdir().unwrap();
    let chief = FakeChief::three();
    let inlet = FakeInlet::new(Some(store_at(tmp.path())));
    inlet.set_chief(Some(chief.clone()));
    let s = serve(tmp.path(), Some(Arc::clone(&inlet)), LinkChatOptions::default()).await;
    let got = jobs(&s).await;
    assert!(ids(&got["jobs"]) == strs(&["c1", "c2", "c3"]) && got["needs_you"] == 3);
    let first = &got["jobs"][0];
    assert_eq!(*first, json!(link_chat::job_entry(&chief.card("c1").unwrap()))); // _as_data, plus what the app needs
    for (k, v) in chief.card("c1").unwrap().data {
        assert_eq!(first[&k], v, "{k}");
    }
    assert!(first["line"] == "Waiting on your Go" && first["needs_you"] == true && first["actions"] == json!(["go", "drop"]));
    assert!(first["steps"] == 3 && first["steps_done"] == 2 && first["rev"] == 1);
    chief.tick(60.0);
    assert!(act(&s, "c2", "drop", Body::None).await.1["ok"] == true);
    chief.tick(60.0);
    assert!(act(&s, "c3", "go", Body::None).await.1["ok"] == true); // approved, not yet run: no longer needs you
    let got = jobs(&s).await;
    assert!(ids(&got["jobs"]) == strs(&["c1", "c3", "c2"]) && got["needs_you"] == 1);
    let (c3, c2) = (&got["jobs"][1], &got["jobs"][2]);
    assert!(c3["needs_you"] == false && c3["actions"] == json!(["drop"]));
    assert!(c2["line"] == "Dropped" && c2["needs_you"] == false && c2["actions"] == json!(["hide"]));
    inlet.set_chief(None);
    let off = s.get(link_chat::JOBS_PATH).await;
    assert!(off.status == 503 && off.json()["error"] == link_chat::NO_CHIEF);
    let off = s.post(&format!("{}/c1/go", link_chat::JOBS_PATH), json!({"rev": 1})).await;
    assert!(off.status == 503 && off.json()["error"] == link_chat::NO_CHIEF); // the body was read and dropped first
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_finished_job_can_be_removed_from_work_for_good() {
    // Owner, 2026-10-10: "i should also have ability to delete completed jobs i dont want to be forced to see them".
    // A finished card leaves the list, stays gone after a restart, and stays in the chief's ledger.
    let tmp = tempfile::tempdir().unwrap();
    let chief = FakeChief::three();
    let inlet = FakeInlet::new(Some(store_at(tmp.path())));
    inlet.set_chief(Some(chief.clone()));
    let s = serve(tmp.path(), Some(inlet), LinkChatOptions::default()).await;
    assert_eq!(act(&s, "c2", "hide", Body::None).await.1, json!({"ok": false, "reason": "c2 is still open; drop it first"}));
    act(&s, "c2", "drop", Body::None).await;
    assert!(ids(&jobs(&s).await["jobs"]).contains(&"c2".to_string()));
    assert_eq!(act(&s, "c2", "hide", Body::None).await.1, json!({"ok": true, "hidden": "c2"}));
    assert!(!ids(&jobs(&s).await["jobs"]).contains(&"c2".to_string()));
    assert_eq!(chief.status("c2"), "dropped"); // the chief's record is whole
    assert_eq!(act(&s, "c99", "hide", Body::None).await.0, 404);
    s.close().await;
    let again = link_chat::HiddenJobs::new(Some(&tmp.path().join("hidden-jobs.json"))); // a restart reads it back
    assert_eq!(again.ids(), ["c2".to_string()].into_iter().collect());
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(tmp.path().join("hidden-jobs.json")).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn each_action_is_the_chiefs_own_method() {
    let tmp = tempfile::tempdir().unwrap();
    let chief = FakeChief::three();
    let inlet = FakeInlet::new(Some(store_at(tmp.path())));
    inlet.set_chief(Some(chief.clone()));
    let s = serve(tmp.path(), Some(inlet), LinkChatOptions::default()).await;
    let (status, r) = act(&s, "c1", "go", Body::None).await;
    assert!(status == 200 && r == json!({"ok": true, "id": "c1", "phase": 3}));
    assert_eq!(chief.calls.lock()[0], ("approve".to_string(), "c1".to_string(), Some("id".to_string()), None)); // as "go c1"
    assert_eq!(act(&s, "c2", "raise", Body::None).await.1, json!({"ok": true, "id": "c2", "usd": 10.0, "minutes": 60}));
    assert_eq!(act(&s, "c2", "drop", Body::None).await.1, json!({"ok": true, "id": "c2"}));
    assert_eq!(chief.status("c2"), "dropped");
    assert_eq!(act(&s, "c2", "drop", Body::None).await.1["ok"], false); // the chief's own refusal
    assert_eq!(act(&s, "c3", "done", Body::None).await.1, json!({"ok": true, "id": "c3"}));
    assert_eq!(chief.status("c3"), "done");
    let (status, r) = act(&s, "c99", "go", Body::None).await;
    assert!(status == 404 && r == json!({"ok": false, "reason": "there is no card c99"}));
    assert_eq!(s.post(&format!("{}/c1/delete", link_chat::JOBS_PATH), json!({})).await.status, 404);
    assert_eq!(s.get(&format!("{}/c1/go", link_chat::JOBS_PATH)).await.status, 405);
    assert_eq!(s.post(link_chat::JOBS_PATH, json!({"x": 1})).await.status, 400); // New job: no goal
    assert_eq!(s.req("PUT", link_chat::JOBS_PATH, Body::None).await.status, 405);
    assert_eq!(act(&s, "c1", "go", Body::Json(json!({"rev": "1"}))).await.0, 400);
    assert_eq!(act(&s, "c1", "go", Body::Json(json!({"rev": true}))).await.0, 400);
    assert_eq!(act(&s, "c1", "go", Body::Json(json!({"rev": 1.0}))).await.0, 400);
    assert_eq!(act(&s, "c1", "go", Body::Raw(b"[1]".to_vec())).await.0, 400);
    assert_eq!(chief.verbs(), strs(&["approve c1", "raise c2", "drop c2", "drop c2", "done c3"])); // nothing else ran
    chief.fail.store(true, Ordering::SeqCst);
    let (status, r) = act(&s, "c1", "happened", Body::None).await;
    assert!(status == 500 && r == json!({"ok": false, "reason": "I couldn't do that just now."}));
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_go_from_the_app_carries_the_rev_the_app_showed() {
    let tmp = tempfile::tempdir().unwrap();
    let chief = FakeChief::three();
    let inlet = FakeInlet::new(Some(store_at(tmp.path())));
    inlet.set_chief(Some(chief.clone()));
    let s = serve(tmp.path(), Some(inlet), LinkChatOptions::default()).await;
    chief.change("c1", |c| c.rev = 2);
    let (status, r) = act(&s, "c1", "go", Body::Json(json!({"rev": 1}))).await; // the card the app showed
    assert!(status == 200 && r["ok"] == false && r["reason"].as_str().unwrap().contains("changed"));
    assert!(chief.card("c1").unwrap().phases[2].approval.is_none());
    assert_eq!(jobs(&s).await["jobs"][0]["rev"], 2);
    assert!(act(&s, "c1", "go", Body::Json(json!({"rev": 2}))).await.1["ok"] == true); // positive control
    assert!(act(&s, "c2", "go", Body::Json(json!({"rev": null}))).await.1["ok"] == true); // null is no rev
    let revs: Vec<Option<i64>> = chief.calls.lock().iter().map(|c| c.3).collect();
    assert_eq!(revs, vec![Some(1), Some(2), None]);
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_act_that_may_have_happened_gets_no_go_only_the_question() {
    let tmp = tempfile::tempdir().unwrap();
    let chief = FakeChief::three();
    for cid in ["c1", "c2"] {
        chief.change(cid, |c| {
            c.doubtful = true;
            c.status = "running".into();
            c.waiting_for = None;
            c.phases[2].status = "running".into();
        });
    }
    let inlet = FakeInlet::new(Some(store_at(tmp.path())));
    inlet.set_chief(Some(chief.clone()));
    let s = serve(tmp.path(), Some(inlet), LinkChatOptions::default()).await;
    let first = jobs(&s).await["jobs"][0].clone();
    assert!(first["id"] == "c1" && first["line"] == "May have happened: check" && first["needs_you"] == true);
    assert_eq!(first["actions"], json!(["happened", "didnt", "drop"])); // never "go"
    let (status, r) = act(&s, "c1", "go", Body::None).await;
    assert!(status == 200 && r["ok"] == false && r["reason"].as_str().unwrap().contains("may already have done"));
    let rev = chief.card("c1").unwrap().rev;
    assert_eq!(act(&s, "c1", "go", Body::Json(json!({"rev": rev}))).await.1["ok"], false);
    assert!(chief.calls.lock().is_empty()); // nothing approved: the chief was never asked
    assert!(act(&s, "c1", "didnt", Body::None).await.1["ok"] == true); // it didn't: a new Go is asked
    let after = jobs(&s).await["jobs"].clone();
    let c1 = after.as_array().unwrap().iter().find(|j| j["id"] == "c1").unwrap().clone();
    assert_eq!(c1["actions"], json!(["go", "drop"]));
    assert!(act(&s, "c1", "go", Body::None).await.1["ok"] == true); // positive control: now it may
    assert!(act(&s, "c2", "happened", Body::None).await.1["ok"] == true); // it happened: taken as done
    assert_eq!(act(&s, "c3", "happened", Body::None).await.1["ok"], false); // c3 has nothing in doubt
    s.close().await;
}

// ---- New job: starting a card from the Work tab ----------------------------------------------------------------

const GOAL: &str = "find me a standing desk under $600 and order it";

async fn new_job(s: &Server, body: Body) -> (u16, Value) {
    let got = s.req("POST", link_chat::JOBS_PATH, body).await;
    (got.status, got.json())
}

/// The turn as the brain would take it: the chief's own take_on with the words heard, then its backbrief.
fn take_on_script(file: bool, reply: bool) -> Script {
    script(move |inlet: Arc<FakeInlet>, app: String| async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        let chief = inlet.chief.lock().clone().unwrap();
        let filed = if file {
            let fake = chief.cards().len() + 1;
            let id = format!("c{fake}");
            Some(id)
        } else {
            None
        };
        if let Some(id) = &filed {
            CHIEFS.lock().get(&(Arc::as_ptr(&chief) as *const () as usize)).unwrap().take_on("Standing desk");
            inlet.filed.lock().entry(app.clone()).or_insert_with(|| id.clone()); // as TelegramInlet._chief_tool records it
        }
        if reply {
            let line = if filed.is_some() { "On it: Standing desk. I'll research, weigh the options, then ask before ordering." } else { "Which desk do you mean?" };
            inlet.out(&app, line, "say");
        }
        inlet.busy.store(false, Ordering::SeqCst);
    })
}

/// The fake chiefs by address, so a script holding the `dyn Chief` can file on the fake.
static CHIEFS: std::sync::LazyLock<Mutex<HashMap<usize, Arc<FakeChief>>>> = std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

fn fresh_chief() -> Arc<FakeChief> {
    let chief = FakeChief::new();
    let as_dyn: Arc<dyn Chief> = chief.clone();
    CHIEFS.lock().insert(Arc::as_ptr(&as_dyn) as *const () as usize, chief.clone());
    chief
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_job_is_heard_as_an_app_turn_and_answers_the_card_it_filed() {
    let tmp = tempfile::tempdir().unwrap();
    let chief = fresh_chief();
    let inlet = FakeInlet::with(Some(store_at(tmp.path())), take_on_script(true, true));
    inlet.set_chief(Some(chief.clone()));
    let s = serve(tmp.path(), Some(Arc::clone(&inlet)), fast(0.01)).await;
    let (status, got) = new_job(&s, Body::Json(json!({"goal": format!("  {GOAL} "), "detail": "  black, 120 cm  "}))).await;
    assert!(status == 200 && got["ok"] == true, "{got}");
    let heard = inlet.heard();
    assert_eq!(heard.len(), 1);
    let (words, on_call, app) = &heard[0];
    assert_eq!(*words, format!("{}{GOAL}\n\n{}black, 120 cm", link_chat::JOB_LEAD, link_chat::JOB_DETAIL_LEAD));
    assert!(!on_call && app.is_some()); // typed, and an app turn's
    let card = chief.card("c1").unwrap();
    assert_eq!(got["job"], json!(link_chat::job_entry(&card))); // GET's shape for one job
    assert!(got["job"]["id"] == "c1" && got["job"]["title"] == "Standing desk");
    assert!(got["said"][0].as_str().unwrap().starts_with("On it: Standing desk."));
    assert_eq!(*inlet.delivered.lock(), vec![true]); // read here
    assert_eq!(ids(&jobs(&s).await["jobs"]), strs(&["c1"]));
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_job_the_brain_does_not_file_says_so_and_files_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let chief = fresh_chief();
    let inlet = FakeInlet::with(Some(store_at(tmp.path())), take_on_script(false, true));
    inlet.set_chief(Some(chief.clone()));
    let s = serve(tmp.path(), Some(Arc::clone(&inlet)), fast(0.01)).await;
    let (status, got) = new_job(&s, Body::Json(json!({"goal": GOAL}))).await;
    assert_eq!(status, 200);
    assert_eq!(got, json!({"ok": false, "said": ["Which desk do you mean?"], "reason": "eve answered without starting a job"}));
    assert!(chief.cards().is_empty() && inlet.heard().len() == 1);
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_job_retried_with_its_client_id_is_heard_once() {
    let tmp = tempfile::tempdir().unwrap();
    let chief = fresh_chief();
    let inlet = FakeInlet::with(Some(store_at(tmp.path())), take_on_script(true, true));
    inlet.set_chief(Some(chief.clone()));
    let s = serve(tmp.path(), Some(Arc::clone(&inlet)), fast(0.01)).await;
    let body = json!({"goal": GOAL, "client_id": "job-1234abcd"});
    let (first, second) = tokio::join!(new_job(&s, Body::Json(body.clone())), new_job(&s, Body::Json(body.clone())));
    assert!(first.1["job"]["id"] == "c1" && second.1["job"]["id"] == "c1", "{} {}", first.1, second.1);
    let third = new_job(&s, Body::Json(body.clone())).await; // after it finished: the same card
    assert!(third.1["ok"] == true && third.1["job"]["id"] == "c1");
    assert!(inlet.heard().len() == 1 && chief.cards().len() == 1);
    let other = new_job(&s, Body::Json(json!({"goal": GOAL, "client_id": "job-5678efgh"}))).await; // positive control
    assert!(other.1["job"]["id"] == "c2" && inlet.heard().len() == 2);
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_job_answers_the_card_its_own_turn_filed_not_a_newer_one() {
    // Gap 2 (2026-10-09): while the New job's turn runs, another turn (a chat turn) files a card after it. The
    // endpoint answers the card its own turn's take_on filed, never the newest.
    let tmp = tempfile::tempdir().unwrap();
    let chief = fresh_chief();
    let c = chief.clone();
    let inlet = FakeInlet::with(
        Some(store_at(tmp.path())),
        script(move |inlet: Arc<FakeInlet>, app: String| {
            let c = c.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(10)).await;
                let mine = c.take_on("Standing desk");
                inlet.filed.lock().entry(app.clone()).or_insert(mine);
                c.tick(60.0); // a minute later, another turn files its own card
                let other = c.take_on("Dentist");
                inlet.filed.lock().entry("another-turn-0001".into()).or_insert(other);
                tokio::time::sleep(Duration::from_millis(50)).await;
                inlet.out(&app, "On it: Standing desk.", "say");
                inlet.busy.store(false, Ordering::SeqCst);
            }
        }),
    );
    inlet.set_chief(Some(chief.clone()));
    let s = serve(tmp.path(), Some(Arc::clone(&inlet)), fast(0.01)).await;
    let (status, got) = new_job(&s, Body::Json(json!({"goal": GOAL}))).await;
    assert!(status == 200 && got["ok"] == true, "{got}");
    assert_eq!(chief.cards().iter().map(|c| c.id.clone()).collect::<Vec<_>>(), strs(&["c1", "c2"]));
    assert!(got["job"]["id"] == "c1" && got["job"]["title"] == "Standing desk"); // its own, not the newest
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_job_whose_turn_filed_nothing_is_not_handed_another_turns_card() {
    let tmp = tempfile::tempdir().unwrap();
    let chief = fresh_chief();
    let c = chief.clone();
    let inlet = FakeInlet::with(
        Some(store_at(tmp.path())),
        script(move |inlet: Arc<FakeInlet>, app: String| {
            let c = c.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(10)).await;
                let other = c.take_on("Standing desk");
                inlet.filed.lock().entry("another-turn-0001".into()).or_insert(other);
                inlet.out(&app, "Which desk do you mean?", "say");
                inlet.busy.store(false, Ordering::SeqCst);
            }
        }),
    );
    inlet.set_chief(Some(chief.clone()));
    let s = serve(tmp.path(), Some(inlet), fast(0.01)).await;
    let (status, got) = new_job(&s, Body::Json(json!({"goal": GOAL}))).await;
    assert_eq!(status, 200);
    assert_eq!(got, json!({"ok": false, "said": ["Which desk do you mean?"], "reason": "eve answered without starting a job"}));
    assert_eq!(chief.cards().len(), 1); // positive control: it was filed
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_job_still_running_at_its_limit_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let chief = fresh_chief();
    let never = script(|_inlet: Arc<FakeInlet>, _app: String| async move {
        tokio::time::sleep(Duration::from_secs(30)).await;
    });
    let inlet = FakeInlet::with(Some(store_at(tmp.path())), never);
    inlet.set_chief(Some(chief.clone()));
    let opts = LinkChatOptions { settle_secs: 0.01, job_secs: 0.2, ..LinkChatOptions::default() };
    let s = serve(tmp.path(), Some(inlet), opts).await;
    let (status, got) = new_job(&s, Body::Json(json!({"goal": GOAL}))).await;
    assert!(status == 200 && got["ok"] == false && got["reason"].as_str().unwrap().contains("still on it"));
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_job_that_cannot_start_is_a_500() {
    let tmp = tempfile::tempdir().unwrap();
    let chief = fresh_chief();
    let inlet = FakeInlet::with(Some(store_at(tmp.path())), take_on_script(true, true));
    inlet.hear_fails.store(true, Ordering::SeqCst);
    inlet.set_chief(Some(chief.clone()));
    let s = serve(tmp.path(), Some(inlet), fast(0.01)).await;
    assert_eq!(new_job(&s, Body::Json(json!({"goal": GOAL}))).await, (500, json!({"ok": false, "reason": "eve couldn't start that just now."})));
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_limits_on_a_new_job() {
    let cases: Vec<(Body, u16)> = vec![
        (Body::Json(json!({})), 400),
        (Body::Json(json!({"goal": ""})), 400),
        (Body::Json(json!({"goal": "   "})), 400),
        (Body::Json(json!({"goal": 5})), 400),
        (Body::Json(json!({"goal": "x".repeat(link_chat::MAX_GOAL_CHARS + 1)})), 400),
        (Body::Json(json!({"goal": "ok", "detail": "x".repeat(link_chat::MAX_DETAIL_CHARS + 1)})), 400),
        (Body::Json(json!({"goal": "ok", "detail": ["no"]})), 400),
        (Body::Json(json!({"goal": "ok", "client_id": "x".repeat(link_chat::MAX_CLIENT_ID_CHARS + 1)})), 400),
        (Body::Json(json!({"goal": "ok", "client_id": null})), 400),
        (Body::Raw(b"[1]".to_vec()), 400),
        (Body::Raw(b"not json".to_vec()), 400),
        (Body::Json(json!({"goal": "x".repeat(link_chat::MAX_BODY_BYTES as usize + 10)})), 413),
    ];
    for (n, (body, status)) in cases.into_iter().enumerate() {
        let tmp = tempfile::tempdir().unwrap();
        let chief = fresh_chief();
        let inlet = FakeInlet::with(Some(store_at(tmp.path())), take_on_script(true, true));
        inlet.set_chief(Some(chief.clone()));
        let s = serve(tmp.path(), Some(Arc::clone(&inlet)), LinkChatOptions::default()).await;
        assert_eq!(s.req("POST", link_chat::JOBS_PATH, body).await.status, status, "case {n}");
        assert!(inlet.heard().is_empty() && chief.cards().is_empty(), "case {n}");
        s.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_job_needs_the_token_the_chief_and_the_chat() {
    let tmp = tempfile::tempdir().unwrap();
    let chief = fresh_chief();
    let inlet = FakeInlet::with(Some(store_at(tmp.path())), take_on_script(true, true));
    inlet.set_chief(Some(chief.clone()));
    let s = serve(tmp.path(), Some(Arc::clone(&inlet)), fast(0.01)).await;
    let body = || Body::Json(json!({"goal": GOAL}));
    assert_eq!(http(s.port, "POST", link_chat::JOBS_PATH, body(), None, None).await.status, 401);
    assert_eq!(http(s.port, "POST", link_chat::JOBS_PATH, body(), Some(&s.token[..s.token.len() - 1]), None).await.status, 401);
    assert_eq!(http(s.port, "POST", link_chat::JOBS_PATH, body(), Some(&s.token), Some("null")).await.status, 403);
    inlet.set_chief(None);
    assert_eq!(new_job(&s, body()).await, (503, json!({"ok": false, "error": link_chat::NO_CHIEF})));
    inlet.set_chief(Some(chief.clone()));
    inlet.off.store(true, Ordering::SeqCst);
    assert_eq!(new_job(&s, body()).await, (503, json!({"ok": false, "error": link_chat::NO_CHAT})));
    assert!(inlet.heard().is_empty() && chief.cards().is_empty());
    inlet.off.store(false, Ordering::SeqCst);
    assert_eq!(new_job(&s, body()).await.1["ok"], true); // positive control
    s.close().await;
}

// ---- the owner's apps ---------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn apps_are_listed_and_open_with_a_fresh_app_token() {
    let tmp = tempfile::tempdir().unwrap();
    let s = serve(tmp.path(), Some(FakeInlet::new(Some(store_at(tmp.path())))), LinkChatOptions::default()).await;
    *s.mini.store.lock() = Some(app_store());
    let got = s.get(link_chat::APPS_PATH).await;
    assert_eq!(got.status, 200);
    assert_eq!(
        got.json(),
        json!({"apps": [{"slug": "plant-log", "name": "Plant log", "updated": 1_790_000_100.0},
                        {"slug": "tip-jar", "name": "Tip jar", "updated": 1_790_000_000.0}]})
    );
    let got = s.post(&format!("{}/tip-jar/open", link_chat::APPS_PATH), json!({})).await;
    assert!(got.status == 200 && got.json()["url"] == "http://127.0.0.1/apps/tip-jar/?t=fresh.tip-jar");
    *s.mini.public_url.lock() = Some("https://example.trycloudflare.com".into());
    let got = s.post(&format!("{}/tip-jar/open", link_chat::APPS_PATH), json!({})).await;
    assert!(got.json()["url"].as_str().unwrap().starts_with("https://example.trycloudflare.com/apps/tip-jar/?t="));
    for slug in ["nope", "..", "tip.jar"] {
        assert_eq!(s.post(&format!("{}/{slug}/open", link_chat::APPS_PATH), json!({})).await.status, 404, "{slug}");
    }
    assert_eq!(s.get(&format!("{}/tip-jar/open", link_chat::APPS_PATH)).await.status, 405);
    *s.mini.store.lock() = None;
    let got = s.get(link_chat::APPS_PATH).await;
    assert!(got.status == 503 && got.json()["error"] == link_chat::NO_APPS);
    s.close().await;
}

// ---- the controls: lights, music, the TV remote ------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn controls_need_the_link_token_and_no_origin() {
    let tmp = tempfile::tempdir().unwrap();
    let s = serve(tmp.path(), Some(FakeInlet::new(Some(store_at(tmp.path())))), LinkChatOptions::default()).await;
    let tv = FakePanel::new(json!({"ok": true, "line": "Home."}));
    s.mini.set("firetv", tv.clone());
    let doors = [("GET", link_chat::CONTROLS_PATH.to_string()), ("POST", format!("{}/firetv", link_chat::CONTROLS_PATH))];
    let home = || Body::Json(json!({"action": "home"}));
    for (method, path) in &doors {
        assert_eq!(http(s.port, method, path, home(), None, None).await.status, 401, "{path}");
        assert_eq!(http(s.port, method, path, home(), Some(&s.token[..s.token.len() - 1]), None).await.status, 401);
        assert_eq!(http(s.port, method, path, home(), Some(&s.token), Some("https://evil.example")).await.status, 403);
        assert_eq!(http(s.port, method, path, home(), Some(&s.token), Some("null")).await.status, 403);
    }
    assert!(tv.bodies().is_empty()); // nothing was pressed
    for (method, path) in &doors {
        assert_eq!(s.req(method, path, home()).await.status, 200, "{path}"); // positive controls
    }
    assert_eq!(tv.bodies(), vec![json!({"action": "home"})]);
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn controls_list_what_this_mac_has() {
    let tmp = tempfile::tempdir().unwrap();
    let s = serve(tmp.path(), Some(FakeInlet::new(Some(store_at(tmp.path())))), LinkChatOptions::default()).await;
    let got = s.get(link_chat::CONTROLS_PATH).await;
    assert!(got.status == 200 && got.json() == Value::Object(link_chat::controls_listing(&|_| None)));
    assert_eq!(got.json()["lights"], json!([]));
    assert_eq!(got.json()["apps"], json!([]));
    let lights = FakePanel::build(json!({}), vec![json!({"name": "desk lamp", "room": "office"}), json!({"name": "strip"})], 0.0, false, None);
    s.mini.set("lights", lights);
    s.mini.set("spotify", FakePanel::new(json!({})));
    let got = s.get(link_chat::CONTROLS_PATH).await.json();
    assert_eq!(got["lights"], json!([{"name": "desk lamp", "room": "office"}, {"name": "strip"}]));
    assert!(got["spotify"] == true && got["firetv"] == false);
    let mini = Arc::clone(&s.mini);
    assert_eq!(got, Value::Object(link_chat::controls_listing(&|n| mini.hub(n)))); // what this Mac has, read now
    let memory = FakePanel::new(json!({}));
    memory.available.store(false, Ordering::SeqCst);
    s.mini.set("memory", memory.clone());
    assert_eq!(s.get(link_chat::CONTROLS_PATH).await.json()["memory"], false);
    assert_eq!(s.post(&format!("{}/memory", link_chat::CONTROLS_PATH), json!({"action": "profile"})).await.status, 404);
    memory.available.store(true, Ordering::SeqCst);
    assert_eq!(s.get(link_chat::CONTROLS_PATH).await.json()["memory"], true);
    assert_eq!(s.post(link_chat::CONTROLS_PATH, json!({})).await.status, 405);
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unknown_control_or_a_missing_device_is_a_404() {
    let tmp = tempfile::tempdir().unwrap();
    let s = serve(tmp.path(), Some(FakeInlet::new(Some(store_at(tmp.path())))), LinkChatOptions::default()).await;
    for name in ["stick", "LIGHTS", "lights/x", "apps", "../firetv"] {
        assert_eq!(s.post(&format!("{}/{name}", link_chat::CONTROLS_PATH), json!({})).await.status, 404, "{name}");
    }
    for name in link_chat::CONTROLS.iter() {
        // none on this Mac
        let got = s.post(&format!("{}/{name}", link_chat::CONTROLS_PATH), json!({"action": "status"})).await;
        assert!(got.status == 404 && got.json() == json!({"ok": false, "line": link_chat::NO_CONTROL[name.as_str()]}), "{name}");
    }
    s.mini.set("lights", FakePanel::new(json!({"ok": true})));
    assert_eq!(s.get(&format!("{}/lights", link_chat::CONTROLS_PATH)).await.status, 405);
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_control_is_the_cards_own_panel_body_in_json_out() {
    let tmp = tempfile::tempdir().unwrap();
    let s = serve(tmp.path(), Some(FakeInlet::new(Some(store_at(tmp.path())))), LinkChatOptions::default()).await;
    let mut answers: HashMap<String, Value> = HashMap::from([
        ("lights".to_string(), json!({"ok": true, "line": "Desk lamp is red.", "failed": []})),
        ("memory".to_string(), json!({"ok": true, "hits": [], "recalled": ["(2026-10-01, app) likes tea"], "said": [], "line": ""})),
        ("notes".to_string(), json!({"ok": true, "notes": [], "folders": ["01-inbox"], "line": ""})),
        ("spotify".to_string(), json!({"ok": true, "playing": true, "track": "A song", "artists": ["Someone"],
                                       "art": "https://i.scdn.co/image/x", "line": "Playing."})),
        ("firetv".to_string(), json!({"ok": true, "power": "on", "volume": 40, "muted": false, "now": "TV on, volume 40%",
                                      "line": "Volume 40%.", "apps": ["YouTube"]})),
        ("pika".to_string(), json!({"ok": true, "line": ""})),
        ("obsbot".to_string(), json!({"ok": true, "line": "Zoom 40%.", "zoom": 40})),
    ]);
    let mut bodies: HashMap<String, Value> = HashMap::from([
        ("pika".to_string(), json!({"action": "drive", "dir": "forward", "hold": "h1"})),
        ("obsbot".to_string(), json!({"action": "zoom", "level": 40})),
        ("lights".to_string(), json!({"action": "set", "target": "desk lamp", "color": "#ff0000", "brightness": 60})),
        ("memory".to_string(), json!({"action": "search", "query": "tea"})),
        ("notes".to_string(), json!({"action": "list"})),
        ("spotify".to_string(), json!({"action": "volume", "volume": 30})),
        ("firetv".to_string(), json!({"action": "volume_change", "change": 5})),
    ]);
    for name in link_chat::CONTROLS.iter() {
        // an app added since: its generic sheet's status read
        answers.entry(name.clone()).or_insert(json!({"ok": true, "ui": 1, "status": "On"}));
        bodies.entry(name.clone()).or_insert(json!({"action": "status"}));
    }
    let panels: HashMap<String, Arc<FakePanel>> = answers.iter().map(|(n, a)| (n.clone(), FakePanel::new(a.clone()))).collect();
    for (name, panel) in &panels {
        s.mini.set(name, panel.clone());
    }
    for name in link_chat::CONTROLS.iter() {
        let got = s.post(&format!("{}/{name}", link_chat::CONTROLS_PATH), bodies[name].clone()).await;
        assert!(got.status == 200 && got.json() == answers[name], "{name}");
        assert_eq!(panels[name].bodies(), vec![bodies[name].clone()], "{name}");
    }
    let path = format!("{}/firetv", link_chat::CONTROLS_PATH);
    let got = s.req("POST", &path, Body::Raw(Vec::new())).await;
    assert!(got.status == 200 && panels["firetv"].bodies().last() == Some(&json!({}))); // no body is a status read
    assert_eq!(s.req("POST", &path, Body::Json(json!([1]))).await.status, 400);
    assert_eq!(s.req("POST", &path, Body::Json(json!({"x": "y".repeat(70_000)}))).await.status, 413);
    assert_eq!(panels["firetv"].bodies().len(), 2); // refused bodies never reach it
    s.mini.set("lights", FakePanel::build(json!({}), Vec::new(), 0.0, true, None));
    let got = s.post(&format!("{}/lights", link_chat::CONTROLS_PATH), json!({"action": "status"})).await;
    assert!(got.status == 500 && got.json() == json!({"ok": false, "line": "That didn't work just now."}));
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_picture_stream_is_the_cards_own_and_behind_the_link_token() {
    let tmp = tempfile::tempdir().unwrap();
    let s = serve(tmp.path(), Some(FakeInlet::new(Some(store_at(tmp.path())))), LinkChatOptions::default()).await;
    for (slot, path) in link_chat::STREAM_PATHS.iter().map(|(p, slot)| (slot.clone(), p.clone())) {
        assert_eq!(http(s.port, "GET", &path, Body::None, None, None).await.status, 401, "{path}");
        let got = s.get(&path).await; // not on this Mac
        assert!(got.status == 404 && got.json() == json!({"ok": false, "line": link_chat::NO_CONTROL[slot.as_str()]}), "{path}");
        let picture = format!("--frame {slot}").into_bytes();
        let card = FakePanel::build(json!({}), Vec::new(), 0.0, false, Some(picture.clone()));
        s.mini.set(&slot, card.clone());
        assert_eq!(http(s.port, "GET", &path, Body::None, Some(&s.token), Some("null")).await.status, 403);
        let got = s.get(&path).await;
        assert!(got.status == 200 && got.headers["content-type"].contains("multipart/x-mixed-replace") && got.body == picture);
        assert_eq!(s.post(&path, json!({})).await.status, 405);
        card.available.store(false, Ordering::SeqCst);
        assert_eq!(s.get(&path).await.status, 404); // switched off: as if absent
    }
    assert!(link_chat::STREAM_PATHS.contains_key(link_chat::PIKA_STREAM_PATH));
    assert!(link_chat::STREAM_PATHS.contains_key(link_chat::OBSBOT_STREAM_PATH));
    s.close().await;
}

// ---- Updates (updates.py): the pane left of Chat --------------------------------------------------------------

/// The suggester's model, slow enough to see a refresh in flight; one Pay card citing the first item.
struct SlowUpdatesClient {
    calls: AtomicUsize,
}

#[async_trait]
impl SuggestClient for SlowUpdatesClient {
    async fn complete(&self, _instructions: &str, body: &str) -> anyhow::Result<String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(300)).await;
        let inner = body.split_once("<snapshot>\n").unwrap().1;
        let inner = inner.rsplit_once("\n</snapshot>").unwrap().0;
        let snapshot: Value = serde_json::from_str(inner)?;
        let reference = snapshot["items"][0]["id"].clone();
        Ok(json!({"cards": [{"verb": "Pay", "text": "the water bill of $64 by Oct 20", "entity": "City Water",
                             "refs": [reference], "due": "2026-10-20"}]})
        .to_string())
    }
}

/// Updates' sources: one note in a vault, nothing else.
struct OneNote(PathBuf);

#[async_trait]
impl UpdateSources for OneNote {
    fn apps(&self, _app: &str) -> Option<Arc<dyn DirectApps>> {
        None
    }
    fn vault(&self) -> Option<PathBuf> {
        let root = self.0.join("vault");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("bills.md"), "# Bills\n- [ ] Pay the water bill by Oct 20\n").unwrap();
        Some(root)
    }
    fn chief(&self) -> Option<Arc<dyn Chief>> {
        None
    }
    fn records_dir(&self) -> Option<PathBuf> {
        None
    }
    fn composio(&self) -> anyhow::Result<Option<Arc<dyn eve_link::seams::ComposioRead>>> {
        Ok(None)
    }
    fn hidden(&self) -> anyhow::Result<Option<eve_transcripts::transcripts::Pred>> {
        Ok(None)
    }
}

/// No sources at all (the default door's Updates, off).
struct NoSources;

#[async_trait]
impl UpdateSources for NoSources {
    fn apps(&self, _app: &str) -> Option<Arc<dyn DirectApps>> {
        None
    }
    fn vault(&self) -> Option<PathBuf> {
        None
    }
    fn chief(&self) -> Option<Arc<dyn Chief>> {
        None
    }
    fn records_dir(&self) -> Option<PathBuf> {
        None
    }
    fn composio(&self) -> anyhow::Result<Option<Arc<dyn eve_link::seams::ComposioRead>>> {
        Ok(None)
    }
    fn hidden(&self) -> anyhow::Result<Option<eve_transcripts::transcripts::Pred>> {
        Ok(None)
    }
}

fn updates_at(tmp: &Path, client: Arc<SlowUpdatesClient>, env: HashMap<String, String>) -> Arc<Updates> {
    let client: Arc<dyn SuggestClient> = client;
    Arc::new(Updates::new(UpdatesOptions {
        path: Some(tmp.join("updates.json")),
        sources: Some(Arc::new(OneNote(tmp.to_path_buf()))),
        client: Some(Arc::new(move || Some(Arc::clone(&client)))),
        environ: Some(env),
        ..UpdatesOptions::default()
    }))
}

async fn until_rebuilt(u: &Updates) {
    for _ in 0..200 {
        if !u.refreshing() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the rebuild never ended");
}

fn one_card() -> Vec<Obj> {
    vec![obj_of(json!({"id": "abc123abc123", "verb": "Pay", "text": "x", "refs": ["note:bills.md"], "kind": "note", "source": "bills"}))]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn updates_need_the_link_token_and_no_origin() {
    let tmp = tempfile::tempdir().unwrap();
    let u = updates_at(tmp.path(), Arc::new(SlowUpdatesClient { calls: AtomicUsize::new(0) }), HashMap::new());
    u.store.put_cards(one_card(), Some("2026-10-12T09:00:00-07:00"), None, None).unwrap();
    let opts = LinkChatOptions { updates: Some(Arc::clone(&u)), ..LinkChatOptions::default() };
    let s = serve(tmp.path(), Some(FakeInlet::new(Some(store_at(tmp.path())))), opts).await;
    let doors = [
        ("GET", link_chat::UPDATES_PATH, None),
        ("POST", link_chat::UPDATES_DISMISS_PATH, Some(json!({"id": "abc123abc123"}))),
        ("POST", link_chat::UPDATES_REFRESH_PATH, Some(json!({}))),
    ];
    let body = |b: &Option<Value>| b.clone().map(Body::Json).unwrap_or(Body::None);
    for (method, path, b) in &doors {
        assert_eq!(http(s.port, method, path, body(b), None, None).await.status, 401, "{path}");
        assert_eq!(http(s.port, method, path, body(b), Some(&s.token[..s.token.len() - 1]), None).await.status, 401);
        assert_eq!(http(s.port, method, path, body(b), Some(&s.token), Some("https://evil.example")).await.status, 403);
        assert_eq!(http(s.port, method, path, body(b), Some(&s.token), Some("null")).await.status, 403);
    }
    assert!(!u.refreshing() && u.store.load().dismissed.is_empty()); // nothing was done
    assert_eq!(s.post(link_chat::UPDATES_PATH, json!({})).await.status, 405);
    assert_eq!(s.get(link_chat::UPDATES_REFRESH_PATH).await.status, 405);
    for (method, path, b) in &doors {
        assert_eq!(s.req(method, path, body(b)).await.status, 200, "{path}"); // positive controls
    }
    assert!(u.store.load().dismissed.contains_key("note:bills.md"));
    until_rebuilt(&u).await;
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn updates_serve_the_contract_dismiss_and_refresh_without_waiting() {
    let tmp = tempfile::tempdir().unwrap();
    let client = Arc::new(SlowUpdatesClient { calls: AtomicUsize::new(0) });
    let u = updates_at(tmp.path(), Arc::clone(&client), HashMap::new());
    let opts = LinkChatOptions { updates: Some(Arc::clone(&u)), ..LinkChatOptions::default() };
    let s = serve(tmp.path(), Some(FakeInlet::new(Some(store_at(tmp.path())))), opts).await;
    let got = s.get(link_chat::UPDATES_PATH).await;
    assert!(got.status == 200 && got.json() == json!({"cards": [], "generated_at": null, "refreshing": false}));
    let started = Instant::now();
    let got = s.post(link_chat::UPDATES_REFRESH_PATH, json!({})).await;
    assert!(got.status == 200 && got.json() == json!({"ok": true, "started": true}));
    assert!(started.elapsed() < Duration::from_millis(250)); // it never waits on the model
    let got = s.req("POST", link_chat::UPDATES_REFRESH_PATH, Body::Raw(Vec::new())).await;
    assert_eq!(got.json(), json!({"ok": true, "started": false})); // one at a time
    assert_eq!(s.get(link_chat::UPDATES_PATH).await.json()["refreshing"], true);
    until_rebuilt(&u).await;
    let view = s.get(link_chat::UPDATES_PATH).await.json();
    assert!(view["refreshing"] == false && view["generated_at"].is_string() && client.calls.load(Ordering::SeqCst) == 1);
    let card = view["cards"][0].clone();
    assert_eq!(view["cards"].as_array().unwrap().len(), 1);
    assert!(card["verb"] == "Pay" && card["kind"] == "note" && card["source"] == "bills" && card["due"] == "2026-10-20");
    for bad in [json!({}), json!({"id": 5}), json!({"id": ""}), json!({"id": "x".repeat(65)}), json!([1])] {
        assert_eq!(s.post(link_chat::UPDATES_DISMISS_PATH, bad.clone()).await.status, 400, "{bad}");
    }
    let got = s.post(link_chat::UPDATES_DISMISS_PATH, json!({"id": "feedfeedfeed"})).await;
    assert!(got.status == 404 && got.json()["ok"] == false); // nothing dismissed: said so
    assert!(u.store.load().dismissed.is_empty());
    let got = s.post(link_chat::UPDATES_DISMISS_PATH, json!({"id": card["id"]})).await;
    assert!(got.status == 200 && got.json() == json!({"ok": true}));
    assert_eq!(s.get(link_chat::UPDATES_PATH).await.json()["cards"], json!([]));
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn updates_off_answer_empty_and_read_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let client = Arc::new(SlowUpdatesClient { calls: AtomicUsize::new(0) });
    let env = HashMap::from([("EVE_UPDATES".to_string(), "0".to_string())]);
    let u = updates_at(tmp.path(), Arc::clone(&client), env);
    u.store.put_cards(one_card(), Some("2026-10-12T09:00:00-07:00"), None, None).unwrap();
    let opts = LinkChatOptions { updates: Some(Arc::clone(&u)), ..LinkChatOptions::default() };
    let s = serve(tmp.path(), Some(FakeInlet::new(Some(store_at(tmp.path())))), opts).await;
    let got = s.get(link_chat::UPDATES_PATH).await;
    assert!(got.status == 200 && got.json() == json!({"cards": [], "generated_at": null, "refreshing": false}));
    let got = s.post(link_chat::UPDATES_REFRESH_PATH, json!({})).await;
    assert!(got.json() == json!({"ok": true, "started": false}) && client.calls.load(Ordering::SeqCst) == 0);
    s.close().await;
}

// ---- the phone's location (phone_location.py) -------------------------------------------------------------

fn location(minutes_ago: f64, extra: Value) -> Value {
    let at = chrono::Utc::now() - chrono::Duration::milliseconds((minutes_ago * 60_000.0) as i64);
    let mut out = json!({"lat": 0.0, "lon": 0.0, "accuracy_m": 15.0, "at": at.to_rfc3339(), "place": "Null Island",
                         "speed_mps": null});
    if let (Value::Object(o), Value::Object(e)) = (&mut out, extra) {
        o.extend(e);
    }
    out
}

fn fix_file(tmp: &Path) -> PathBuf {
    tmp.join("phone-location.json")
}

/// The log lines a test's door writes, captured (the door's tasks run on this thread).
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
    type Writer = Captured;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

fn capture() -> (Captured, tracing::subscriber::DefaultGuard) {
    let out = Captured::default();
    // No timestamp: a fractional second like "41.87" would otherwise match the coordinates this log must never say.
    let sub = tracing_subscriber::fmt().with_writer(out.clone()).with_ansi(false).without_time().with_max_level(tracing::Level::DEBUG).finish();
    let guard = tracing::subscriber::set_default(sub);
    (out, guard)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_phone_location_needs_the_link_token_and_no_origin() {
    let tmp = tempfile::tempdir().unwrap();
    let s = serve(tmp.path(), Some(FakeInlet::new(Some(store_at(tmp.path())))), LinkChatOptions::default()).await;
    let path = link_chat::LOCATION_PATH;
    let fix = || Body::Json(location(1.0, json!({})));
    assert_eq!(http(s.port, "POST", path, fix(), None, None).await.status, 401); // no token
    assert_eq!(http(s.port, "POST", path, fix(), Some(&s.token[..s.token.len() - 1]), None).await.status, 401);
    for origin in ["https://evil.example", "null"] {
        assert_eq!(http(s.port, "POST", path, fix(), Some(&s.token), Some(origin)).await.status, 403);
    }
    assert!(eve_places::phone_location::load(Some(&fix_file(tmp.path()))).is_none()); // nothing was kept
    assert_eq!(s.get(path).await.status, 405); // POST only
    let got = s.req("POST", path, fix()).await; // positive control
    assert!(got.status == 200 && got.json() == json!({"ok": true}));
    assert_eq!(eve_places::phone_location::load(Some(&fix_file(tmp.path()))).unwrap().place.as_deref(), Some("Null Island"));
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_body_that_is_not_a_fix_is_a_400_and_keeps_nothing() {
    let bads: Vec<Body> = vec![
        Body::Raw(Vec::new()),
        Body::Raw(b"{not json".to_vec()),
        Body::Raw(b"[1, 2]".to_vec()),
        Body::Json(location(1.0, json!({"lat": 91}))),
        Body::Json(location(1.0, json!({"lon": "0"}))),
        Body::Json(location(1.0, json!({"accuracy_m": -3}))),
        Body::Json(location(1.0, json!({"at": "2026-10-12T17:55:00"}))),
        Body::Json(location(-30.0, json!({}))),
        Body::Json(location(1.0, json!({"place": 5}))),
        Body::Json(location(1.0, json!({"speed_mps": "x"}))),
        Body::Json(json!({"lat": 0.0, "lon": 0.0})),
        // too big for a float: a 400, not a dropped socket
        Body::Raw(format!("{{\"lat\": 1{}, \"lon\": 0.0, \"accuracy_m\": 15.0, \"at\": \"2026-10-12T17:55:00+00:00\"}}", "0".repeat(400)).into_bytes()),
    ];
    for (n, bad) in bads.into_iter().enumerate() {
        let tmp = tempfile::tempdir().unwrap();
        let s = serve(tmp.path(), Some(FakeInlet::new(Some(store_at(tmp.path())))), LinkChatOptions::default()).await;
        let got = s.req("POST", link_chat::LOCATION_PATH, bad).await;
        assert_eq!(got.status, 400, "case {n}");
        let body = got.json();
        assert!(body["ok"] == false && !body["error"].as_str().unwrap().is_empty(), "case {n}");
        assert!(!fix_file(tmp.path()).exists(), "case {n}");
        s.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_body_too_big_is_a_413() {
    let tmp = tempfile::tempdir().unwrap();
    let s = serve(tmp.path(), Some(FakeInlet::new(Some(store_at(tmp.path())))), LinkChatOptions::default()).await;
    let big = location(1.0, json!({"place": "x".repeat(link_chat::MAX_BODY_BYTES as usize + 1)}));
    assert_eq!(s.post(link_chat::LOCATION_PATH, big).await.status, 413);
    s.close().await;
}

#[tokio::test(flavor = "current_thread")]
async fn an_older_fix_is_ignored_and_the_log_never_says_where() {
    let (said, _guard) = capture();
    let tmp = tempfile::tempdir().unwrap();
    let s = serve(tmp.path(), Some(FakeInlet::new(Some(store_at(tmp.path())))), LinkChatOptions::default()).await;
    let new = location(1.0, json!({"lat": 37.7749, "lon": -122.4194, "place": "San Francisco, CA"}));
    assert_eq!(s.post(link_chat::LOCATION_PATH, new).await.status, 200);
    let old = location(10.0, json!({"lat": 1.0, "lon": 1.0, "place": "Null Island"}));
    let got = s.post(link_chat::LOCATION_PATH, old).await;
    assert!(got.status == 200 && got.json() == json!({"ok": true}));
    s.close().await;
    let kept = eve_places::phone_location::load(Some(&fix_file(tmp.path()))).unwrap();
    assert!(kept.lat == 37.7749 && kept.place.as_deref() == Some("San Francisco, CA"));
    let said = String::from_utf8(said.0.lock().clone()).unwrap();
    assert!(said.contains("location updated") && said.contains("location ignored") && said.contains("accuracy 15 m"), "{said}");
    for secret in ["37.77", "122.4", "San Francisco", "Null Island", "1.0, 1.0"] {
        assert!(!said.contains(secret), "{secret} in: {said}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_switch_off_is_a_403_and_keeps_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let opts = LinkChatOptions { environ: Some(env_at(tmp.path(), &[("EVE_PHONE_LOCATION", "0")])), ..LinkChatOptions::default() };
    let s = serve(tmp.path(), Some(FakeInlet::new(Some(store_at(tmp.path())))), opts).await;
    let got = s.post(link_chat::LOCATION_PATH, location(1.0, json!({}))).await;
    assert!(got.status == 403 && got.json() == json!({"ok": false, "error": link_chat::NO_LOCATION}));
    let fix = Body::Json(location(1.0, json!({})));
    assert_eq!(http(s.port, "POST", link_chat::LOCATION_PATH, fix, None, None).await.status, 401); // the token still first
    s.close().await;
    assert!(!fix_file(tmp.path()).exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fix_that_cannot_be_kept_is_a_500() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a-file"), "not a folder").unwrap();
    let bad = tmp.path().join("a-file").join("phone-location.json");
    let opts = LinkChatOptions {
        environ: Some(env_at(tmp.path(), &[("EVE_PHONE_LOCATION_FILE", bad.to_str().unwrap())])),
        ..LinkChatOptions::default()
    };
    let s = serve(tmp.path(), Some(FakeInlet::new(Some(store_at(tmp.path())))), opts).await;
    let got = s.post(link_chat::LOCATION_PATH, location(1.0, json!({}))).await;
    assert!(got.status == 500 && got.json() == json!({"ok": false, "error": "the Mac could not keep the location"}));
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_location_route_needs_no_chat_running() {
    let tmp = tempfile::tempdir().unwrap();
    let inlet = FakeInlet::new(Some(store_at(tmp.path())));
    inlet.off.store(true, Ordering::SeqCst); // the chat never started: still kept
    let s = serve(tmp.path(), Some(inlet), LinkChatOptions::default()).await;
    assert_eq!(s.post(link_chat::LOCATION_PATH, location(1.0, json!({}))).await.status, 200);
    s.close().await;
    assert!(eve_places::phone_location::load(Some(&fix_file(tmp.path()))).is_some());
}

// ---- the places history (places.py) ------------------------------------------------------------------------------

fn a_visit(extra: Value) -> Value {
    let arrived = chrono::Utc::now() - chrono::Duration::hours(1);
    let mut out = json!({"lat": 41.87, "lon": -87.64, "accuracy_m": 20.0, "place": "Somewhere St", "poi": "Equinox",
                         "category": "fitnessCenter", "arrived": arrived.to_rfc3339(), "departed": null});
    if let (Value::Object(o), Value::Object(e)) = (&mut out, extra) {
        o.extend(e);
    }
    out
}

#[tokio::test(flavor = "current_thread")]
async fn visits_and_fixes_build_the_history_and_the_log_never_says_where() {
    let (said, _guard) = capture();
    let tmp = tempfile::tempdir().unwrap();
    let s = serve(tmp.path(), Some(FakeInlet::new(Some(store_at(tmp.path())))), LinkChatOptions::default()).await;
    assert_eq!(http(s.port, "POST", link_chat::VISIT_PATH, Body::Json(a_visit(json!({}))), None, None).await.status, 401);
    let arrival = a_visit(json!({}));
    let got = s.post(link_chat::VISIT_PATH, arrival.clone()).await;
    assert!(got.status == 200 && got.json() == json!({"ok": true}));
    let mut left = arrival.clone();
    left["departed"] = json!(chrono::Utc::now().to_rfc3339());
    assert_eq!(s.post(link_chat::VISIT_PATH, left).await.status, 200);
    assert_eq!(s.post(link_chat::VISIT_PATH, a_visit(json!({"lat": 99}))).await.status, 400);
    assert_eq!(s.post(link_chat::LOCATION_PATH, location(1.0, json!({"lat": 41.87, "lon": -87.64}))).await.status, 200);
    let got = s.get(link_chat::PLACES_PATH).await;
    let body = got.json();
    assert!(got.status == 200 && body["ok"] == true && body["on"] == true && body["places"] == 1 && body["visits"] == 1, "{body}");
    assert_eq!(s.get(link_chat::VISIT_PATH).await.status, 405); // POST only
    let got = s.post(link_chat::PLACES_FORGET_PATH, json!({})).await;
    assert!(got.status == 200 && got.json()["forgot"]["visits"] == 1);
    s.close().await;
    let env = env_at(tmp.path(), &[]);
    let summary = eve_places::places::Places::new(None, Some(env), None).summary().unwrap();
    assert_eq!(Value::Object(summary), json!({"places": 0, "visits": 0, "things": 0, "since": null}));
    let said = String::from_utf8(said.0.lock().clone()).unwrap();
    assert!(said.contains("places: visit arrival") && said.contains("places: visit departure"), "{said}");
    for secret in ["41.87", "87.64", "Equinox", "Somewhere St"] {
        assert!(!said.contains(secret), "{secret} in: {said}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn places_off_is_a_403_and_keeps_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let opts = LinkChatOptions { environ: Some(env_at(tmp.path(), &[("EVE_PLACES", "0")])), ..LinkChatOptions::default() };
    let s = serve(tmp.path(), Some(FakeInlet::new(Some(store_at(tmp.path())))), opts).await;
    for (method, path, body) in [
        ("POST", link_chat::VISIT_PATH, Body::Json(a_visit(json!({})))),
        ("GET", link_chat::PLACES_PATH, Body::None),
        ("POST", link_chat::PLACES_FORGET_PATH, Body::Json(json!({}))),
    ] {
        let got = s.req(method, path, body).await;
        assert!(got.status == 403 && got.json() == json!({"ok": false, "error": link_chat::NO_PLACES}), "{path}");
    }
    assert_eq!(s.post(link_chat::LOCATION_PATH, location(1.0, json!({}))).await.status, 200);
    s.close().await;
    assert!(!tmp.path().join("places.db").exists()); // the latest fix is kept, the history is not
    assert!(fix_file(tmp.path()).exists());
}

// ---- Work sessions (work_sessions.py) ------------------------------------------------------------------------

const SID: &str = "0f0e0d0c-0b0a-4908-8706-050403020100";

/// work_sessions.WorkSessions as the routes call it, each call recorded.
#[derive(Default)]
struct FakeWork {
    calls: Mutex<Vec<(String, Value)>>,
    stopped: AtomicBool,
}

#[async_trait]
impl WorkDoor for FakeWork {
    async fn list(&self) -> (u16, Value) {
        self.calls.lock().push(("list".into(), Value::Null));
        (200, json!({"ok": true, "sessions": [{"id": SID, "state": "working"}]}))
    }

    async fn start(&self, task: &Value, client_id: &Value) -> (u16, Value) {
        self.calls.lock().push(("start".into(), json!([task, client_id])));
        match task.as_str() {
            Some(t) if !t.trim().is_empty() => (200, json!({"ok": true, "session": {"id": SID, "state": "starting"}})),
            _ => (400, json!({"ok": false, "error": "task must be words"})),
        }
    }

    async fn detail(&self, sid: &str) -> (u16, Value) {
        self.calls.lock().push(("detail".into(), json!(sid)));
        (200, json!({"ok": true, "session": {"id": sid}, "turns": []}))
    }

    async fn reply(&self, sid: &str, text: &Value) -> (u16, Value) {
        self.calls.lock().push(("reply".into(), json!([sid, text])));
        if self.stopped.load(Ordering::SeqCst) {
            return (409, json!({"ok": false, "error": "that session is not running"}));
        }
        (200, json!({"ok": true}))
    }

    async fn stop(&self, sid: &str) -> std::io::Result<(u16, Value)> {
        self.calls.lock().push(("stop".into(), json!(sid)));
        self.stopped.store(true, Ordering::SeqCst);
        Ok((200, json!({"ok": true, "stopped": true})))
    }
}

fn with_work(work: &Arc<FakeWork>) -> LinkChatOptions {
    let w = Arc::clone(work);
    LinkChatOptions { work: Some(Arc::new(move || Some(Arc::clone(&w) as Arc<dyn WorkDoor>))), ..LinkChatOptions::default() }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_session_route_needs_the_link_token_and_no_origin() {
    let tmp = tempfile::tempdir().unwrap();
    let work = Arc::new(FakeWork::default());
    let s = serve(tmp.path(), Some(FakeInlet::new(Some(store_at(tmp.path())))), with_work(&work)).await;
    let doors = [
        ("GET", link_chat::SESSIONS_PATH.to_string(), None),
        ("POST", link_chat::SESSION_START_PATH.to_string(), Some(json!({"task": "hi"}))),
        ("GET", format!("{}/{SID}", link_chat::SESSIONS_PATH), None),
        ("POST", format!("{}/{SID}/reply", link_chat::SESSIONS_PATH), Some(json!({"text": "hi"}))),
        ("POST", format!("{}/{SID}/stop", link_chat::SESSIONS_PATH), None),
    ];
    let body = |b: &Option<Value>| b.clone().map(Body::Json).unwrap_or(Body::None);
    for (method, path, b) in &doors {
        assert_eq!(http(s.port, method, path, body(b), None, None).await.status, 401, "{path}");
        assert_eq!(http(s.port, method, path, body(b), Some(&s.token[..s.token.len() - 1]), None).await.status, 401);
        assert_eq!(http(s.port, method, path, body(b), Some(&s.token), Some("https://evil.example")).await.status, 403);
        assert_eq!(http(s.port, method, path, body(b), Some(&s.token), Some("null")).await.status, 403);
    }
    assert!(work.calls.lock().is_empty()); // nothing was started
    let got = s.post(link_chat::SESSION_START_PATH, json!({"task": "hi", "client_id": "a"})).await; // positive control
    assert!(got.status == 200 && got.json()["session"]["state"] == "starting");
    assert_eq!(work.calls.lock()[0], ("start".to_string(), json!(["hi", "a"])));
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_routes_shapes_wrong_methods_and_unknown_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let work = Arc::new(FakeWork::default());
    let s = serve(tmp.path(), Some(FakeInlet::new(Some(store_at(tmp.path())))), with_work(&work)).await;
    let got = s.post(link_chat::SESSION_START_PATH, json!({"task": "Tidy the notes"})).await;
    assert!(got.status == 200 && got.headers["content-type"] == "application/json");
    assert_eq!(work.calls.lock()[0], ("start".to_string(), json!(["Tidy the notes", ""]))); // no client_id: ""
    let listed = s.get(link_chat::SESSIONS_PATH).await.json();
    assert!(listed["ok"] == true && listed["sessions"][0]["id"] == SID);
    let detail = s.get(&format!("{}/{SID}", link_chat::SESSIONS_PATH)).await.json();
    assert!(detail["ok"] == true && detail["turns"] == json!([]) && detail["session"]["id"] == SID);
    assert_eq!(s.post(&format!("{}/{SID}/reply", link_chat::SESSIONS_PATH), json!({"text": "go on"})).await.status, 200);
    assert_eq!(work.calls.lock().last().unwrap().1, json!([SID, "go on"]));
    assert_eq!(s.req("POST", &format!("{}/{SID}/stop", link_chat::SESSIONS_PATH), Body::None).await.status, 200);
    assert_eq!(s.post(&format!("{}/{SID}/reply", link_chat::SESSIONS_PATH), json!({"text": "more"})).await.status, 409);
    for (method, path) in [
        ("POST", link_chat::SESSIONS_PATH.to_string()),
        ("GET", link_chat::SESSION_START_PATH.to_string()),
        ("POST", format!("{}/{SID}", link_chat::SESSIONS_PATH)),
        ("GET", format!("{}/{SID}/reply", link_chat::SESSIONS_PATH)),
        ("GET", format!("{}/{SID}/stop", link_chat::SESSIONS_PATH)),
    ] {
        let body = if method == "POST" { Body::Json(json!({})) } else { Body::None };
        assert_eq!(s.req(method, &path, body).await.status, 405, "{path}");
    }
    for path in [
        format!("{}/not-a-uuid", link_chat::SESSIONS_PATH),
        format!("{}/{SID}/kill", link_chat::SESSIONS_PATH),
        format!("{}/{}", link_chat::SESSIONS_PATH, SID.to_uppercase()),
        format!("{}/../../etc", link_chat::SESSIONS_PATH),
    ] {
        assert_eq!(s.get(&path).await.status, 404, "{path}");
    }
    assert_eq!(s.req("POST", link_chat::SESSION_START_PATH, Body::Raw(b"[1, 2]".to_vec())).await.status, 400);
    assert_eq!(s.post(link_chat::SESSION_START_PATH, json!({"task": ""})).await.status, 400);
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn work_off_is_a_503_on_every_route() {
    for off in [None, Some(())] {
        let tmp = tempfile::tempdir().unwrap();
        let opts = match off {
            None => LinkChatOptions::default(),
            Some(()) => LinkChatOptions { work: Some(Arc::new(|| None)), ..LinkChatOptions::default() },
        };
        let s = serve(tmp.path(), Some(FakeInlet::new(Some(store_at(tmp.path())))), opts).await;
        for (method, path, body) in [
            ("GET", link_chat::SESSIONS_PATH.to_string(), Body::None),
            ("POST", link_chat::SESSION_START_PATH.to_string(), Body::Json(json!({"task": "hi"}))),
            ("POST", format!("{}/{SID}/reply", link_chat::SESSIONS_PATH), Body::Json(json!({"text": "x"}))),
        ] {
            let got = s.req(method, &path, body).await;
            assert!(got.status == 503 && got.json() == json!({"ok": false, "error": link_chat::NO_WORK}), "{path}");
        }
        s.close().await;
    }
}

// ---- the composer's eve/Muse switch --------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_link_door_reads_and_flips_muse() {
    let tmp = tempfile::tempdir().unwrap();
    let inlet = FakeInlet::new(Some(store_at(tmp.path())));
    let s = serve(tmp.path(), Some(Arc::clone(&inlet)), LinkChatOptions::default()).await;
    let available = eve_muse_pi::available();
    assert_eq!(s.get(link_chat::MUSE_PATH).await.json(), json!({"ok": true, "available": available, "on": false}));
    let got = s.post(link_chat::MUSE_PATH, json!({"on": true})).await;
    assert!(got.json() == json!({"ok": true, "available": available, "on": true}) && inlet.muse.load(Ordering::SeqCst));
    assert_eq!(s.post(link_chat::MUSE_PATH, json!({"on": "yes"})).await.status, 400);
    assert_eq!(s.req("PUT", link_chat::MUSE_PATH, Body::None).await.status, 405);
    inlet.off.store(true, Ordering::SeqCst);
    assert_eq!(s.get(link_chat::MUSE_PATH).await.status, 503); // the switch is the chat's
    s.close().await;
}

// ---- uploads (link_uploads.py) ----------------------------------------------------------------------------

async fn upload(port: u16, token: Option<&str>, data: &[u8], name: &str, length: Option<usize>, origin: Option<&str>) -> (u16, Value) {
    let mut conn = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let mut head = format!(
        "POST {} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nX-Filename: {}\r\n",
        link_chat::UPLOAD_PATH,
        length.unwrap_or(data.len()),
        urlencoding::encode(name)
    );
    if let Some(token) = token {
        head += &format!("Authorization: Bearer {token}\r\n");
    }
    if let Some(origin) = origin {
        head += &format!("Origin: {origin}\r\n");
    }
    head += "\r\n";
    conn.write_all(head.as_bytes()).await.unwrap();
    if length.is_none_or(|l| l <= data.len()) {
        conn.write_all(data).await.unwrap();
    }
    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(20), conn.read_to_end(&mut raw)).await.unwrap().unwrap();
    let got = parse(&raw);
    let body = if got.body.is_empty() { json!({}) } else { got.json() };
    (got.status, body)
}

/// A store whose clock runs `skew` seconds ahead of the wall (an expiry without touching mtimes).
fn store_with_skew(root: PathBuf) -> (Arc<UploadStore>, Arc<Mutex<f64>>) {
    let skew = Arc::new(Mutex::new(0.0));
    let s = Arc::clone(&skew);
    let store = UploadStore::new(Some(root)).with_clock(move || {
        let wall = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64();
        wall + *s.lock()
    });
    (Arc::new(store), skew)
}

fn with_uploads(store: &Arc<UploadStore>, settle: f64) -> LinkChatOptions {
    LinkChatOptions { uploads: Some(Arc::clone(store)), settle_secs: settle, ..LinkChatOptions::default() }
}

async fn turn_with(s: &Server, text: &str, ids: Value, client_id: &str) -> Resp {
    s.post(link_chat::TURN_PATH, json!({"text": text, "client_id": client_id, "attachments": ids})).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn upload_needs_the_link_token_and_no_origin() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(UploadStore::new(Some(tmp.path().join("up"))));
    let s = serve(tmp.path(), Some(FakeInlet::new(None)), with_uploads(&store, 0.2)).await;
    assert_eq!(upload(s.port, None, JPEG, "a.jpg", None, None).await.0, 401);
    assert_eq!(upload(s.port, Some(&format!("wrong-token-{}", "x".repeat(30))), JPEG, "a.jpg", None, None).await.0, 401);
    assert_eq!(upload(s.port, Some(&s.token), JPEG, "a.jpg", None, Some("https://evil.example")).await.0, 403);
    let (status, got) = upload(s.port, Some(&s.token), JPEG, "Beach day.jpg", None, None).await; // positive control
    assert!(status == 200 && got["ok"] == true && got["kind"] == "image" && got["name"] == "Beach day.jpg", "{got}");
    assert!(store.get(got["id"].as_str().unwrap()).is_some());
    assert_eq!(s.get(link_chat::UPLOAD_PATH).await.status, 405);
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn upload_size_cap_and_type_refusal() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(UploadStore::new(Some(tmp.path().join("up"))));
    let s = serve(tmp.path(), Some(FakeInlet::new(None)), with_uploads(&store, 0.2)).await;
    let (status, got) = upload(s.port, Some(&s.token), b"", "a.jpg", Some(eve_link_uploads::MAX_BYTES + 1), None).await;
    assert!(status == 413 && got["ok"] == false); // refused on the header, before reading
    let (status, got) = upload(s.port, Some(&s.token), &b"MZ\x90\x00".repeat(10), "tool.exe", None, None).await;
    assert!(status == 415 && got["error"].as_str().unwrap().contains("photos, PDFs"), "{got}");
    assert_eq!(upload(s.port, Some(&s.token), b"", "empty.txt", None, None).await.0, 400);
    assert_eq!(upload(s.port, Some(&s.token), b"%PDF-1.4 small", "ok.pdf", None, None).await.0, 200); // positive control
    let metas = std::fs::read_dir(tmp.path().join("up")).unwrap().filter(|e| {
        e.as_ref().unwrap().file_name().to_string_lossy().ends_with(".meta.json")
    });
    assert_eq!(metas.count(), 1);
    // a lower cap (Python's monkeypatched link_uploads.MAX_BYTES) is the store's own
    let small = Arc::new(UploadStore::new(Some(tmp.path().join("small"))).with_max_bytes(1000));
    let s2 = serve(&tmp.path().join("b"), Some(FakeInlet::new(None)), with_uploads(&small, 0.2)).await;
    assert_eq!(upload(s2.port, Some(&s2.token), &b"a".repeat(1001), "big.txt", Some(1001), None).await.0, 413);
    s.close().await;
    s2.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn upload_with_chunked_encoding_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(UploadStore::new(Some(tmp.path().join("up"))));
    let s = serve(tmp.path(), Some(FakeInlet::new(None)), with_uploads(&store, 0.2)).await;
    let mut conn = TcpStream::connect(("127.0.0.1", s.port)).await.unwrap();
    let head = format!(
        "POST {} HTTP/1.1\r\nAuthorization: Bearer {}\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
        link_chat::UPLOAD_PATH,
        s.token
    );
    conn.write_all(head.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    conn.read_to_end(&mut raw).await.unwrap();
    let got = parse(&raw);
    assert!(got.status == 411 && got.json()["error"] == "send the upload with a Content-Length");
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_names_only_known_live_ids() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, skew) = store_with_skew(tmp.path().join("up"));
    let inlet = FakeInlet::new(None);
    let s = serve(tmp.path(), Some(Arc::clone(&inlet)), with_uploads(&store, 0.2)).await;
    let good = store.save(JPEG, "a.jpg").unwrap();
    for bad in [
        json!(["../../../etc/passwd"]),
        json!(["0".repeat(32)]),
        json!([good.id, good.id]),
        json!([good.id, good.id, good.id, good.id, good.id]),
        json!("not-a-list"),
        json!([7]),
        Value::Null,
    ] {
        let got = turn_with(&s, "look", bad.clone(), "").await;
        assert_eq!(got.status, 400, "{bad}");
    }
    assert_eq!(turn_with(&s, "", json!([]), "").await.status, 400); // no words and nothing attached
    assert!(inlet.heard().is_empty());
    *skew.lock() += eve_link_uploads::KEEP_SECS as f64 + 1.0;
    let got = turn_with(&s, "look", json!([good.id]), "").await;
    assert!(got.status == 400 && String::from_utf8_lossy(&got.body).contains("expired"));
    assert!(inlet.heard().is_empty());
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn attachments_reach_the_inlet_and_an_empty_text_is_fine_with_one() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(UploadStore::new(Some(tmp.path().join("up"))));
    let inlet = FakeInlet::new(None);
    let s = serve(tmp.path(), Some(Arc::clone(&inlet)), with_uploads(&store, 0.01)).await;
    let up = store.save(PDF, "q.pdf").unwrap();
    let got = turn_with(&s, "", json!([up.id]), "c-1").await;
    assert!(got.status == 200 && events(&got.body).1);
    assert!(inlet.heard()[0].0.is_empty());
    assert_eq!(*inlet.attachments.lock(), vec![vec![up]]);
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn photos_and_pdfs_over_the_inline_cap_together_are_refused_before_the_brain() {
    // They go to the model inside the request: past MAX_INLINE_BYTES the turn is a 413 with a line to show.
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(UploadStore::new(Some(tmp.path().join("up"))));
    let inlet = FakeInlet::new(None);
    let s = serve(tmp.path(), Some(Arc::clone(&inlet)), with_uploads(&store, 0.01)).await;
    let mut big = PDF.to_vec();
    big.resize(eve_link_uploads::MAX_BYTES, b' ');
    let a = store.save(&big, "a.pdf").unwrap();
    let b = store.save(JPEG, "b.jpg").unwrap();
    let text = store.save(&b"plain words\n".repeat(50), "notes.txt").unwrap();
    assert!(a.size + b.size > link_chat::MAX_INLINE_BYTES);
    let got = turn_with(&s, "look", json!([a.id, b.id]), "").await;
    assert!(got.status == 413 && String::from_utf8_lossy(&got.body).contains("send fewer at once"));
    assert!(inlet.attachments.lock().is_empty());
    let got = turn_with(&s, "look", json!([a.id, text.id]), "c-ok").await; // a text file is not inlined
    assert_eq!(got.status, 200);
    let names: Vec<Vec<String>> = inlet.attachments.lock().iter().map(|l| l.iter().map(|u| u.name.clone()).collect()).collect();
    assert_eq!(names, vec![strs(&["a.pdf", "notes.txt"])]);
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_expired_attachment_shows_as_gone_in_the_history() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, skew) = store_with_skew(tmp.path().join("up"));
    let transcripts = store_at(tmp.path());
    let up = store.save(JPEG, "old.jpg").unwrap();
    let conv = transcripts.new_conv("telegram");
    let att = Some(link_chat::att_record(std::slice::from_ref(&up)));
    transcripts.append_with("telegram", &conv, "owner", "image", "look", Append { via: "app".into(), att, ..Append::default() });
    let s = serve(tmp.path(), Some(FakeInlet::new(Some(transcripts))), with_uploads(&store, 0.2)).await;
    let first = s.messages("").await["messages"][0].clone();
    let a = &first["attachments"][0];
    assert!(a["thumb"].as_str().is_some_and(|t| t.starts_with("data:image/jpeg;base64,")) && a.get("gone").is_none(), "{first}");
    *skew.lock() += eve_link_uploads::KEEP_SECS as f64 + 1.0;
    let first = s.messages("").await["messages"][0].clone();
    assert_eq!(first["attachments"], json!([{"id": up.id, "name": "old.jpg", "kind": "image", "gone": true}]));
    s.close().await;
}

// ---- GET /api/link/file/<id> ---------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_link_door_lends_its_store_to_the_inlet() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(UploadStore::new(Some(tmp.path().join("door"))));
    let inlet = FakeInlet::new(Some(store_at(tmp.path())));
    let s = serve(tmp.path(), Some(Arc::clone(&inlet)), with_uploads(&store, 0.2)).await;
    s.messages("").await;
    assert!(inlet.link_uploads.lock().as_ref().is_some_and(|u| Arc::ptr_eq(u, &store)));
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_file_door_needs_the_token_and_serves_live_ids_only() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, skew) = store_with_skew(tmp.path().join("up"));
    let up = store.keep(JPEG, "a.jpg").unwrap();
    std::fs::write(tmp.path().join("secret.txt"), "not yours").unwrap();
    let s = serve(tmp.path(), Some(FakeInlet::new(Some(store_at(tmp.path())))), with_uploads(&store, 0.2)).await;
    let path = format!("/api/link/file/{}", up.id);
    assert_eq!(http(s.port, "GET", &path, Body::None, None, None).await.status, 401);
    assert_eq!(http(s.port, "GET", &path, Body::None, Some(&"x".repeat(43)), None).await.status, 401);
    assert_eq!(http(s.port, "GET", &path, Body::None, Some(&s.token), Some("https://evil.example")).await.status, 403);
    assert_eq!(s.post(&path, json!({})).await.status, 405);
    let got = s.get(&path).await; // positive control
    assert!(got.status == 200 && got.body == JPEG && got.headers["x-content-type-options"] == "nosniff");
    for bad in [
        "0".repeat(32),
        up.id.to_uppercase(),
        format!("{}.meta.json", up.id),
        up.id[..31].to_string(),
        "..%2Fsecret.txt".to_string(),
        "%2e%2e%2f%2e%2e%2fsecret.txt".to_string(),
        "../../secret.txt".to_string(),
        "../secret.txt".to_string(),
        String::new(),
        format!("{}/x", up.id),
    ] {
        let got = s.get(&format!("/api/link/file/{bad}")).await;
        assert!(got.status == 404 && !String::from_utf8_lossy(&got.body).contains("not yours"), "{bad}");
    }
    *skew.lock() += eve_link_uploads::KEEP_SECS as f64 + 1.0;
    let got = s.get(&path).await;
    assert!(got.status == 404 && String::from_utf8_lossy(&got.body).contains("gone")); // expired
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_kept_file_is_also_an_owner_upload_s_door() {
    // The owner's own attachments open full size the same way.
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(UploadStore::new(Some(tmp.path().join("up"))));
    let up = store.save(b"milk\neggs\n", "list.txt").unwrap();
    let s = serve(tmp.path(), Some(FakeInlet::new(Some(store_at(tmp.path())))), with_uploads(&store, 0.2)).await;
    let got = s.get(&format!("/api/link/file/{}", up.id)).await;
    assert!(got.status == 200 && got.headers["content-type"] == "text/plain" && got.body == b"milk\neggs\n");
    s.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_file_door_serves_only_a_photo_or_pdf_inline_and_never_an_active_type() {
    let cases: Vec<(&str, Vec<u8>, &str, &str)> = vec![
        ("page.html", b"<script>alert(1)</script>".to_vec(), "application/octet-stream", "attachment"),
        ("run.js", b"alert(1)".to_vec(), "application/octet-stream", "attachment"),
        ("feed.xml", b"<a/>".to_vec(), "application/octet-stream", "attachment"),
        ("photos.zip", [b"PK\x03\x04".to_vec(), vec![0u8; 16]].concat(), "application/zip", "attachment"),
        ("notes.txt", b"milk".to_vec(), "text/plain", "attachment"),
        ("lease.pdf", PDF.to_vec(), "application/pdf", "inline"),
        ("desk.jpg", JPEG.to_vec(), "image/jpeg", "inline"),
    ];
    for (name, data, mime, disposition) in cases {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(UploadStore::new(Some(tmp.path().join("up"))));
        let up = store.keep(&data, name).unwrap();
        let s = serve(tmp.path(), Some(FakeInlet::new(Some(store_at(tmp.path())))), with_uploads(&store, 0.2)).await;
        let got = s.get(&format!("/api/link/file/{}", up.id)).await;
        assert!(got.status == 200 && got.body == data, "{name}");
        assert!(got.headers["content-type"] == mime && got.headers["content-disposition"] == disposition, "{name}");
        assert_eq!(got.headers["x-content-type-options"], "nosniff");
        s.close().await;
    }
}

// ---- Horizontal History's routes (link_topics.py) -------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_topic_routes_list_save_and_refuse() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store_at(tmp.path());
    let day = chrono::Local::now().date_naive() - chrono::Duration::days(1);
    let at = |h: u32, m: u32| eve_transcripts::py::local_aware(NaiveDateTime::new(day, NaiveTime::from_hms_opt(h, m, 0).unwrap()));
    for (who, text, when) in [
        ("owner", "plan the Lisbon trip for early November", at(9, 0)),
        ("eve", "Three days works well.", at(9, 1)),
        ("owner", "hey", at(12, 0)), // small talk: no card
        ("eve", "Hey!", at(12, 0)),
        ("owner", "what's on my calendar for tomorrow?", at(14, 0)),
        ("eve", "Two meetings.", at(14, 0)),
    ] {
        store.append_with("telegram", "t-1", who, "say", text, Append { now: Some(when), ..Append::default() });
    }
    let s = serve(tmp.path(), Some(FakeInlet::new(Some(store))), LinkChatOptions::default()).await;
    let got = s.get(link_chat::TOPICS_PATH).await;
    let view = got.json();
    let titles: Vec<&str> = view["topics"].as_array().unwrap().iter().map(|t| t["title"].as_str().unwrap()).collect();
    assert!(got.status == 200 && titles == ["what's on my calendar for tomorrow?", "plan the Lisbon trip for early November"]);
    let first = view["topics"][1].clone();
    assert!(first["bookmarked"] == false && view["bookmarked"] == json!([]));
    let got = s.post(link_chat::TOPICS_BOOKMARK_PATH, json!({"id": first["id"], "on": true})).await;
    assert!(got.status == 200 && got.json() == json!({"ok": true, "bookmarked": true}));
    let view = s.get(link_chat::TOPICS_PATH).await.json();
    assert_eq!(view["bookmarked"][0]["title"], "plan the Lisbon trip for early November");
    assert_eq!(view["topics"][1]["bookmarked"], true);
    let one = s.get(&format!("{}?limit=1", link_chat::TOPICS_PATH)).await;
    assert!(one.status == 200 && one.json()["topics"].as_array().unwrap().len() == 1);
    assert_eq!(s.get(&format!("{}?limit=x", link_chat::TOPICS_PATH)).await.status, 400);
    for bad in [json!({}), json!({"id": "../etc", "on": true}), json!({"id": first["id"], "on": "yes"})] {
        assert_eq!(s.post(link_chat::TOPICS_BOOKMARK_PATH, bad.clone()).await.status, 400, "{bad}");
    }
    assert_eq!(s.post(link_chat::TOPICS_BOOKMARK_PATH, json!({"id": "2026-01-01:0", "on": true})).await.status, 404);
    assert_eq!(http(s.port, "GET", link_chat::TOPICS_PATH, Body::None, None, None).await.status, 401); // no token
    assert_eq!(http(s.port, "GET", link_chat::TOPICS_PATH, Body::None, Some(&s.token), Some("https://example.com")).await.status, 403);
    assert_eq!(s.post(link_chat::TOPICS_PATH, json!({})).await.status, 405);
    s.close().await;
}

// ---- the small helpers -----------------------------------------------------------------------------------

#[tokio::test]
async fn write_json_is_pythons_json_with_its_head() {
    let mut out: Vec<u8> = Vec::new();
    LinkChat::write_json(&mut out, 409, &json!({"ok": false, "error": "café"})).await.unwrap();
    let text = String::from_utf8(out).unwrap();
    let body = "{\"ok\": false, \"error\": \"caf\\u00e9\"}";
    assert_eq!(
        text,
        format!(
            "HTTP/1.1 409 Conflict\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    );
}

#[tokio::test]
async fn discard_reads_at_most_what_was_said_and_stops_at_eof() {
    static DATA: [u8; 100] = [7u8; 100];
    let mut reader: &'static [u8] = &DATA;
    LinkChat::discard(&mut reader, 40).await;
    assert_eq!(reader.len(), 60);
    LinkChat::discard(&mut reader, -1).await; // not a number: nothing read
    assert_eq!(reader.len(), 60);
    LinkChat::discard(&mut reader, 1000).await; // the body ends first
    assert!(reader.is_empty());
}

#[test]
fn busy_is_in_flight_and_not_waiting_on_the_owner() {
    let inlet = FakeInlet::new(None);
    assert!(!LinkChat::busy(inlet.as_ref()));
    inlet.busy.store(true, Ordering::SeqCst);
    assert!(LinkChat::busy(inlet.as_ref()));
    inlet.waiting.store(true, Ordering::SeqCst);
    assert!(!LinkChat::busy(inlet.as_ref()));
}
