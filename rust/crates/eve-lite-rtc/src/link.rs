//! The link channel: LinkChat's HTTP as messages ([`LinkChannel`], [`ChannelWriter`]; the crate docs have the
//! protocol).

use std::collections::HashMap;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use futures::future::BoxFuture;
use futures::FutureExt;
use indexmap::IndexMap;
use parking_lot::Mutex;
use regex::Regex;
use serde_json::{json, Map, Value};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use eve_link::seams::Headers;

use crate::channel::{Channel, ChannelEvent, Data};
use crate::hub::LinkDoor;
use crate::{
    py, text, BODY_FRAME_BYTES, BUFFER_HIGH, DRAIN_POLL_SECS, DROPPED_REQUEST_HEADERS, DROPPED_RESPONSE_HEADERS,
    HEADER_NAME_RE, LINK_PATHS, MAX_BODY_BYTES, MAX_HEADERS, MAX_HEADER_CHARS, MAX_HEAD_BYTES, MAX_ID,
    MAX_MESSAGE_CHARS, MAX_QUERY_CHARS, MAX_REQUESTS,
};

static HEADER_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(HEADER_NAME_RE).expect("the header name pattern"));

// ---- the request's body ----------------------------------------------------------------------------------------

#[derive(Default)]
struct BodyState {
    buf: Vec<u8>,
    pos: usize,
    eof: bool,
    waker: Option<Waker>,
}

/// What LinkChat reads a request's body from (Python's `asyncio.StreamReader`): the page's body, then nothing until
/// a cancel or a closed channel feeds the EOF. Clones share it.
#[derive(Clone, Default)]
pub struct BodyReader(Arc<Mutex<BodyState>>);

impl BodyReader {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed_data(&self, data: &[u8]) {
        let mut s = self.0.lock();
        s.buf.extend_from_slice(data);
        if let Some(w) = s.waker.take() {
            w.wake();
        }
    }

    pub fn feed_eof(&self) {
        let mut s = self.0.lock();
        s.eof = true;
        if let Some(w) = s.waker.take() {
            w.wake();
        }
    }

    /// The body was read whole and the EOF came.
    pub fn at_eof(&self) -> bool {
        let s = self.0.lock();
        s.eof && s.pos >= s.buf.len()
    }
}

impl AsyncRead for BodyReader {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, out: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let mut s = self.0.lock();
        if s.pos < s.buf.len() {
            let n = (s.buf.len() - s.pos).min(out.remaining());
            let start = s.pos;
            out.put_slice(&s.buf[start..start + n]);
            s.pos += n;
            return Poll::Ready(Ok(()));
        }
        if s.eof {
            return Poll::Ready(Ok(()));
        }
        s.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

// ---- the answer's writer ---------------------------------------------------------------------------------------

/// What LinkChat wrote is not an HTTP/1.1 response this writer can frame.
#[derive(Debug)]
struct BadResponse(#[allow(dead_code)] &'static str);

enum ParseError {
    /// Why is kept for a debugger; the page and the log get only the request's `err`, as in Python.
    Bad(#[allow(dead_code)] BadResponse),
    /// The channel closed under the writer (ConnectionResetError).
    Reset,
}

impl From<BadResponse> for ParseError {
    fn from(b: BadResponse) -> Self {
        ParseError::Bad(b)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Head,
    Length,
    ChunkSize,
    ChunkData,
    ChunkEnd,
    Trailer,
    UntilClose,
    Done,
}

struct WriterState {
    /// cancelled, or the channel closed: nothing more goes out
    dead: bool,
    /// end or err went out
    done: bool,
    status: i128,
    buf: Vec<u8>,
    state: State,
    left: u128,
}

fn reset(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::ConnectionReset, why.to_string())
}

/// What LinkChat writes one request's answer to. It calls only `write` and `flush` (its drain), with HTTP/1.1: the
/// status line and headers, then a body framed by Content-Length or chunked. Parsed as it comes: the head goes out
/// as one `head` message, the body de-chunked as binary frames (the request's id, then at most
/// [`BODY_FRAME_BYTES`]), and its end as `end`. A body that breaks off is `err`. After a cancel or a closed channel
/// every write fails with ConnectionReset, which LinkChat's stream takes as a hang-up. Clones share the answer.
pub struct ChannelWriter {
    channel: LinkChannel,
    pub id: u32,
    prefix: [u8; 4],
    st: Arc<Mutex<WriterState>>,
    draining: Mutex<Option<BoxFuture<'static, ()>>>,
}

impl Clone for ChannelWriter {
    fn clone(&self) -> Self {
        ChannelWriter {
            channel: self.channel.clone(),
            id: self.id,
            prefix: self.prefix,
            st: self.st.clone(),
            draining: Mutex::new(None),
        }
    }
}

impl ChannelWriter {
    pub fn new(channel: &LinkChannel, rid: u32) -> Self {
        ChannelWriter {
            channel: channel.clone(),
            id: rid,
            prefix: rid.to_be_bytes(),
            st: Arc::new(Mutex::new(WriterState {
                dead: false,
                done: false,
                status: 0,
                buf: Vec::new(),
                state: State::Head,
                left: 0,
            })),
            draining: Mutex::new(None),
        }
    }

    pub fn dead(&self) -> bool {
        self.st.lock().dead
    }

    pub fn set_dead(&self) {
        self.st.lock().dead = true;
    }

    pub fn done(&self) -> bool {
        self.st.lock().done
    }

    /// The answer's status, once its head went out (0 before).
    pub fn status(&self) -> i128 {
        self.st.lock().status
    }

    // -- what LinkChat calls --

    /// One write: parsed and sent now. An error is the request's end (the page cancelled, or the channel closed).
    pub fn write(&self, data: &[u8]) -> io::Result<()> {
        let mut st = self.st.lock();
        if st.dead {
            return Err(reset("the page cancelled this request"));
        }
        if st.done {
            return Ok(()); // nothing follows the end of an answer
        }
        st.buf.extend_from_slice(data);
        match self.parse(&mut st) {
            Ok(()) => Ok(()),
            Err(ParseError::Reset) => Err(reset("the link channel closed")),
            Err(ParseError::Bad(_)) => {
                warn!("lite rtc: a link answer could not be framed; the request ends in err");
                self.finish_with(&mut st, "err");
                st.dead = true; // and LinkChat's next write ends its stream
                Err(reset("the answer could not be framed"))
            }
        }
    }

    /// Wait while the channel is behind (the page reads slowly).
    pub async fn drain(&self) -> io::Result<()> {
        if self.dead() {
            return Err(reset("the page cancelled this request"));
        }
        self.channel.drained().await;
        if self.dead() {
            return Err(reset("the page cancelled this request"));
        }
        Ok(())
    }

    /// asyncio.StreamWriter's `is_closing`, so a LinkChat that one day closes its writer still ends the answer.
    pub fn is_closing(&self) -> bool {
        let st = self.st.lock();
        st.dead || st.done
    }

    /// `close`: the answer ends as a closed connection would end it.
    pub fn close(&self) {
        self.finish();
        self.set_dead();
    }

    // -- the request's end, seen from LinkChannel --

    /// LinkChat returned: an answer whose length is the connection's ends now; one cut short is `err`.
    pub fn finish(&self) {
        let mut st = self.st.lock();
        if st.done || st.dead {
            return;
        }
        if st.state == State::UntilClose {
            self.finish_with(&mut st, "end");
        } else {
            self.finish_with(&mut st, "err");
        }
    }

    pub fn fail(&self) {
        let mut st = self.st.lock();
        if !st.done && !st.dead {
            self.finish_with(&mut st, "err");
        }
    }

    // -- the parser --

    fn parse(&self, st: &mut WriterState) -> Result<(), ParseError> {
        let mut body: Vec<u8> = Vec::new();
        loop {
            let state = st.state;
            match state {
                State::Head => {
                    let Some(end) = find(&st.buf, b"\r\n\r\n") else {
                        if st.buf.len() > MAX_HEAD_BYTES {
                            return Err(BadResponse("head too long").into());
                        }
                        break;
                    };
                    let head: Vec<u8> = st.buf[..end].to_vec();
                    st.buf.drain(..end + 4);
                    self.head(st, &head)?;
                }
                State::Length | State::ChunkData => {
                    let take = st.left.min(st.buf.len() as u128) as usize;
                    body.extend_from_slice(&st.buf[..take]);
                    st.buf.drain(..take);
                    st.left -= take as u128;
                    if st.left > 0 {
                        break;
                    }
                    st.state = if state == State::Length { State::Done } else { State::ChunkEnd };
                }
                State::ChunkSize => {
                    let Some(end) = find(&st.buf, b"\r\n") else {
                        if st.buf.len() > 1024 {
                            return Err(BadResponse("chunk size line too long").into());
                        }
                        break;
                    };
                    let raw: Vec<u8> = st.buf[..end].to_vec();
                    st.buf.drain(..end + 2);
                    let line = raw.split(|c| *c == b';').next().unwrap_or(&[]);
                    let line = py::strip_bytes(line);
                    // int(bytes, 16): ASCII only, and no whitespace inside past what bytes.strip() took
                    let ascii = line.is_ascii() && !line.iter().any(|c| (0x1c..=0x1f).contains(c));
                    let size = std::str::from_utf8(line)
                        .ok()
                        .filter(|_| ascii)
                        .and_then(|s| py::int(s, 16))
                        .ok_or(BadResponse("bad chunk size"))?;
                    if size < 0 {
                        return Err(BadResponse("bad chunk size").into());
                    }
                    st.left = size as u128;
                    st.state = if size > 0 { State::ChunkData } else { State::Trailer };
                }
                State::ChunkEnd => {
                    if st.buf.len() < 2 {
                        break;
                    }
                    if &st.buf[..2] != b"\r\n" {
                        return Err(BadResponse("a chunk without its CRLF").into());
                    }
                    st.buf.drain(..2);
                    st.state = State::ChunkSize;
                }
                State::Trailer => {
                    let Some(end) = find(&st.buf, b"\r\n") else {
                        if st.buf.len() > MAX_HEAD_BYTES {
                            return Err(BadResponse("trailer too long").into());
                        }
                        break;
                    };
                    let empty = end == 0;
                    st.buf.drain(..end + 2);
                    if empty {
                        st.state = State::Done;
                    }
                }
                State::UntilClose => {
                    body.append(&mut st.buf);
                    break;
                }
                State::Done => {
                    st.buf.clear(); // anything after the answer is not part of it
                    break;
                }
            }
        }
        if !body.is_empty() {
            self.frames(st, &body)?;
        }
        if st.state == State::Done && !st.done {
            self.finish_with(st, "end");
        }
        Ok(())
    }

    fn head(&self, st: &mut WriterState, raw: &[u8]) -> Result<(), ParseError> {
        let decoded: String = raw.iter().map(|b| *b as char).collect(); // latin-1
        let lines: Vec<&str> = decoded.split("\r\n").collect();
        let top: Vec<&str> = lines[0].splitn(3, ' ').collect();
        if top.len() < 2 || !top[0].starts_with("HTTP/") {
            return Err(BadResponse("no status line").into());
        }
        let status = py::int(top[1], 10).ok_or(BadResponse("bad status"))?;
        let mut headers: Vec<(String, String)> = Vec::new();
        for line in &lines[1..] {
            let Some((name, value)) = line.split_once(':') else {
                return Err(BadResponse("bad header line").into());
            };
            let name = py::strip(name).to_lowercase();
            let value = py::strip(value).to_string();
            match headers.iter_mut().find(|(k, _)| *k == name) {
                Some(slot) => slot.1 = format!("{}, {}", slot.1, value),
                None => headers.push((name, value)),
            }
        }
        let get = |k: &str| headers.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        let chunked = get("transfer-encoding").unwrap_or_default().to_lowercase().contains("chunked");
        if chunked {
            headers.retain(|(k, _)| k != "content-length"); // chunked wins (RFC 9112 6.3); the frames carry it all
        }
        st.status = status;
        let mut h = Map::new();
        for (k, v) in &headers {
            if !DROPPED_RESPONSE_HEADERS.contains(&k.as_str()) {
                h.insert(k.clone(), Value::String(v.clone()));
            }
        }
        let status_value = i64::try_from(status).map(Value::from).unwrap_or_else(|_| Value::from(status as f64));
        self.send(st, Data::Text(text(&json!({"t": "head", "id": self.id, "s": status_value, "h": Value::Object(h)}))))?;
        let length = headers.iter().find(|(n, _)| n == "content-length").map(|(_, v)| v.clone());
        if (100..200).contains(&status) || status == 204 || status == 304 {
            st.state = State::Done;
        } else if chunked {
            st.state = State::ChunkSize;
        } else if let Some(length) = length {
            let left = py::int(&length, 10).ok_or(BadResponse("bad content-length"))?;
            if left < 0 {
                return Err(BadResponse("bad content-length").into());
            }
            st.left = left as u128;
            st.state = if left > 0 { State::Length } else { State::Done };
        } else {
            st.state = State::UntilClose;
        }
        Ok(())
    }

    fn frames(&self, st: &mut WriterState, body: &[u8]) -> Result<(), ParseError> {
        for piece in body.chunks(BODY_FRAME_BYTES) {
            let mut frame = Vec::with_capacity(4 + piece.len());
            frame.extend_from_slice(&self.prefix);
            frame.extend_from_slice(piece);
            self.send(st, Data::Binary(frame))?;
        }
        Ok(())
    }

    fn finish_with(&self, st: &mut WriterState, kind: &str) {
        st.done = true;
        self.channel.send(Data::Text(text(&json!({"t": kind, "id": self.id}))));
    }

    /// One message for this request. The channel gone: the request is dead and LinkChat's write fails.
    fn send(&self, st: &mut WriterState, data: Data) -> Result<(), ParseError> {
        if !self.channel.send(data) {
            st.dead = true;
            return Err(ParseError::Reset);
        }
        Ok(())
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

impl AsyncWrite for ChannelWriter {
    fn poll_write(self: Pin<&mut Self>, _cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        Poll::Ready(self.write(buf).map(|()| buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut draining = self.draining.lock();
        if draining.is_none() {
            if self.dead() {
                return Poll::Ready(Err(reset("the page cancelled this request")));
            }
            let channel = self.channel.clone();
            *draining = Some(async move { channel.drained().await }.boxed());
        }
        let fut = draining.as_mut().expect("a drain is running");
        match fut.as_mut().poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(()) => {
                *draining = None;
                drop(draining);
                if self.dead() {
                    Poll::Ready(Err(reset("the page cancelled this request")))
                } else {
                    Poll::Ready(Ok(()))
                }
            }
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.close();
        Poll::Ready(Ok(()))
    }
}

// ---- one request ----------------------------------------------------------------------------------------------

/// One request running on a channel: its body's reader, its answer's writer, and the task serving it.
pub struct LinkRequest {
    pub id: u32,
    pub reader: BodyReader,
    pub writer: ChannelWriter,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl LinkRequest {
    /// The serving task has ended (returned, or cancelled).
    pub fn task_done(&self) -> bool {
        self.task.lock().as_ref().is_none_or(|t| t.is_finished())
    }
}

fn request_id(raw: Option<&Value>) -> Option<u32> {
    match raw {
        Some(Value::Number(n)) => n.as_u64().filter(|v| *v <= MAX_ID).map(|v| v as u32),
        _ => None,
    }
}

/// The page's headers, lowercased, without the ones the door replaces or drops; None when malformed.
fn request_headers(raw: Option<&Value>) -> Option<Headers> {
    let raw = match raw {
        None | Some(Value::Null) => return Some(HashMap::new()),
        Some(Value::Object(m)) if m.len() <= MAX_HEADERS => m,
        Some(_) => return None,
    };
    let mut out = HashMap::new();
    for (name, value) in raw {
        let Value::String(value) = value else { return None };
        let name = py::strip(name).to_lowercase();
        if !HEADER_NAME.is_match(&name)
            || value.chars().count() > MAX_HEADER_CHARS
            || value.contains('\r')
            || value.contains('\n')
            || value.contains('\0')
        {
            return None;
        }
        if !DROPPED_REQUEST_HEADERS.contains(&name.as_str()) {
            out.insert(name, py::strip(value).to_string());
        }
    }
    Some(out)
}

// ---- the channel ------------------------------------------------------------------------------------------------

/// Told of each request that reaches LinkChat, by its path (RtcHub's counts).
pub type Served = Arc<dyn Fn(&str) + Send + Sync>;

struct LinkInner {
    channel: Arc<dyn Channel>,
    link: Option<Arc<dyn LinkDoor>>,
    token: Vec<u8>,
    bearer: String,
    served: Option<Served>,
    max_requests: usize,
    requests: Mutex<IndexMap<u32, Arc<LinkRequest>>>,
    closed: AtomicBool,
    low: Notify,
}

/// One `link` data channel: the page's HTTP requests, answered by the unchanged LinkChat (crate docs). `token` is
/// the peer's secret: each request reaches LinkChat with `Authorization: Bearer <token>` and that as its only
/// accepted token. `served(path)` is told of each request that reaches LinkChat. Clones share the channel.
#[derive(Clone)]
pub struct LinkChannel {
    inner: Arc<LinkInner>,
}

impl LinkChannel {
    pub fn new(channel: Arc<dyn Channel>, link: Option<Arc<dyn LinkDoor>>, token: &str) -> Self {
        Self::with(channel, link, token, None, MAX_REQUESTS)
    }

    pub fn with(
        channel: Arc<dyn Channel>,
        link: Option<Arc<dyn LinkDoor>>,
        token: &str,
        served: Option<Served>,
        max_requests: usize,
    ) -> Self {
        let lc = LinkChannel {
            inner: Arc::new(LinkInner {
                channel: channel.clone(),
                link,
                token: token.as_bytes().to_vec(),
                bearer: format!("Bearer {token}"),
                served,
                max_requests,
                requests: Mutex::new(IndexMap::new()),
                closed: AtomicBool::new(false),
                low: Notify::new(),
            }),
        };
        channel.set_buffered_amount_low_threshold(BUFFER_HIGH);
        let weak = Arc::downgrade(&lc.inner);
        channel.on(Arc::new(move |event| {
            let Some(inner) = weak.upgrade() else { return };
            let lc = LinkChannel { inner };
            match event {
                ChannelEvent::BufferedAmountLow => lc.on_low(),
                ChannelEvent::Message(m) => lc.on_message(m),
                ChannelEvent::Close => lc.close(),
            }
        }));
        lc
    }

    /// The data channel under it.
    pub fn channel(&self) -> &Arc<dyn Channel> {
        &self.inner.channel
    }

    pub fn closed(&self) -> bool {
        self.inner.closed.load(Ordering::SeqCst)
    }

    pub fn max_requests(&self) -> usize {
        self.inner.max_requests
    }

    /// The ids of the requests running now, oldest first.
    pub fn requests(&self) -> Vec<u32> {
        self.inner.requests.lock().keys().copied().collect()
    }

    pub fn request(&self, rid: u32) -> Option<Arc<LinkRequest>> {
        self.inner.requests.lock().get(&rid).cloned()
    }

    /// LinkChat's token check: the peer's secret, compared in constant time.
    pub fn check(&self, token: &str) -> bool {
        py::compare_digest(token.as_bytes(), &self.inner.token)
    }

    // -- sending --

    pub fn send(&self, data: Data) -> bool {
        !self.closed() && crate::send(self.inner.channel.as_ref(), data)
    }

    fn on_low(&self) {
        self.inner.low.notify_waiters();
    }

    /// Wait while more than [`BUFFER_HIGH`] waits in the channel: a slow page holds LinkChat's writes back.
    pub async fn drained(&self) {
        loop {
            let low = self.inner.low.notified();
            tokio::pin!(low);
            low.as_mut().enable();
            if self.closed() || self.inner.channel.buffered_amount().await <= BUFFER_HIGH {
                return;
            }
            let _ = tokio::time::timeout(Duration::from_secs_f64(DRAIN_POLL_SECS), low).await;
        }
    }

    /// An answer of the door's own, in LinkChat's shape (a JSON body), for a request that never reaches it.
    fn refuse(&self, rid: u32, status: u16, error: &str) {
        let body = py::dumps(&json!({"ok": false, "error": error}), false).into_bytes();
        let head = json!({"t": "head", "id": rid, "s": status,
                          "h": {"content-type": "application/json", "content-length": body.len().to_string(),
                                "cache-control": "no-store"}});
        if self.send(Data::Text(text(&head))) {
            let mut frame = rid.to_be_bytes().to_vec();
            frame.extend_from_slice(&body);
            self.send(Data::Binary(frame));
            self.send(Data::Text(text(&json!({"t": "end", "id": rid}))));
        }
    }

    // -- receiving --

    fn on_message(&self, message: Data) {
        // never into the stack's receive loop
        let run = std::panic::AssertUnwindSafe(|| self.message(message));
        if std::panic::catch_unwind(run).is_err() {
            warn!("lite rtc: a link message failed (Exception)");
        }
    }

    fn message(&self, message: Data) {
        if self.closed() {
            return;
        }
        let message = match message {
            Data::Text(s) if s.len() <= MAX_MESSAGE_CHARS || s.chars().count() <= MAX_MESSAGE_CHARS => s,
            _ => {
                info!("lite rtc: a link message that is not a request was dropped");
                return;
            }
        };
        let msg: Value = match serde_json::from_str(&message) {
            Ok(v) => v,
            Err(_) => {
                info!("lite rtc: a link message that is not JSON was dropped");
                return;
            }
        };
        let Value::Object(msg) = msg else { return };
        match msg.get("t").and_then(Value::as_str) {
            Some("req") => self.request_msg(&msg),
            Some("cancel") => {
                if let Some(rid) = request_id(msg.get("id")) {
                    self.cancel(rid);
                }
            }
            _ => {}
        }
    }

    fn request_msg(&self, msg: &Map<String, Value>) {
        let Some(rid) = request_id(msg.get("id")) else {
            info!("lite rtc: a link request without an id was dropped");
            return;
        };
        if self.inner.requests.lock().contains_key(&rid) {
            info!("lite rtc: a link request reused a running id; dropped");
            return;
        }
        let method = msg.get("m");
        let query = match msg.get("q") {
            None | Some(Value::Null) => Some(String::new()),
            Some(Value::String(s)) => Some(s.clone()),
            Some(_) => None,
        };
        let (Some(Value::String(path)), Some(query)) = (msg.get("p"), query) else {
            self.refuse(rid, 400, "p and q must be strings");
            return;
        };
        let (mut path, mut query) = (path.clone(), query);
        if path.contains('?') && query.is_empty() {
            let (p, q) = path.split_once('?').map(|(p, q)| (p.to_string(), q.to_string())).unwrap_or_default();
            path = p; // the query in the path, as a URL has it
            query = q;
        }
        let query = query.strip_prefix('?').map(str::to_string).unwrap_or(query);
        if !LINK_PATHS.contains(&path.as_str()) {
            self.refuse(rid, 404, "not found");
            return;
        }
        let method = match method {
            Some(Value::String(m)) => m.to_uppercase(),
            _ => String::new(),
        };
        if method != "GET" && method != "POST" {
            self.refuse(rid, 405, "wrong method");
            return;
        }
        if query.chars().count() > MAX_QUERY_CHARS {
            self.refuse(rid, 400, "the query is too long");
            return;
        }
        let Some(mut headers) = request_headers(msg.get("h")) else {
            self.refuse(rid, 400, &format!("h must be at most {MAX_HEADERS} string headers"));
            return;
        };
        let body = match msg.get("b") {
            None | Some(Value::Null) => String::new(),
            Some(_) if method == "GET" => String::new(),
            Some(Value::String(s)) => s.clone(),
            Some(_) => {
                self.refuse(rid, 400, "b must be a string");
                return;
            }
        };
        let data = body.into_bytes();
        if data.len() > MAX_BODY_BYTES {
            self.refuse(rid, 413, &format!("the body must be at most {MAX_BODY_BYTES} bytes here"));
            return;
        }
        if self.inner.requests.lock().len() >= self.inner.max_requests {
            self.refuse(rid, 429, &format!("at most {} requests at once on this channel", self.inner.max_requests));
            return;
        }
        headers.insert("authorization".into(), self.inner.bearer.clone());
        if !data.is_empty() {
            headers.insert("content-length".into(), data.len().to_string());
        }
        let reader = BodyReader::new();
        if !data.is_empty() {
            reader.feed_data(&data); // and no EOF: that means the page hung up
        }
        let req = Arc::new(LinkRequest {
            id: rid,
            reader,
            writer: ChannelWriter::new(self, rid),
            task: Mutex::new(None),
        });
        // the slot and the task under one lock, so a cancel never finds the slot without its task
        let mut requests = self.inner.requests.lock();
        requests.insert(rid, req.clone());
        let me = self.clone();
        let running = req.clone();
        let task = tokio::spawn(async move { me.run(running, method, path, query, headers).await });
        *req.task.lock() = Some(task);
    }

    async fn run(&self, req: Arc<LinkRequest>, method: String, path: String, query: String, headers: Headers) {
        if let Some(served) = &self.inner.served {
            served(&path);
        }
        let result = match &self.inner.link {
            Some(link) => {
                let mut reader = req.reader.clone();
                let mut writer = req.writer.clone();
                let me = self.clone();
                let check = move |t: &str| me.check(t);
                let serving = link.serve(&method, &path, &query, &headers, &mut reader, &mut writer, &check);
                // a panic in LinkChat is its exception: the request ends in err and its slot is freed
                match std::panic::AssertUnwindSafe(serving).catch_unwind().await {
                    Ok(served) => served.map_err(|e| {
                        let kind = match e.kind() {
                            io::ErrorKind::ConnectionReset => "ConnectionResetError",
                            io::ErrorKind::BrokenPipe => "BrokenPipeError",
                            io::ErrorKind::UnexpectedEof => "IncompleteReadError",
                            io::ErrorKind::TimedOut => "TimeoutError",
                            _ => "OSError",
                        };
                        kind.to_string()
                    }),
                    Err(_) => Err("Exception".to_string()),
                }
            }
            None => Err("AttributeError".to_string()),
        };
        match result {
            Err(kind) => {
                if !req.writer.dead() {
                    warn!("lite rtc: a link request failed ({})", kind);
                }
                req.writer.fail();
            }
            Ok(()) => req.writer.finish(),
        }
        let mut requests = self.inner.requests.lock();
        if requests.get(&req.id).is_some_and(|r| Arc::ptr_eq(r, &req)) {
            requests.shift_remove(&req.id);
        }
    }

    /// The page gave up on a request: LinkChat sees its client hang up (EOF), its writes stop, and the slot is free
    /// at once. Nothing more is sent for it.
    pub fn cancel(&self, rid: u32) {
        let req = self.inner.requests.lock().shift_remove(&rid);
        if let Some(req) = req {
            Self::drop_request(&req);
        }
    }

    fn drop_request(req: &LinkRequest) {
        req.writer.set_dead();
        req.reader.feed_eof();
        let task = req.task.lock();
        if let Some(task) = task.as_ref() {
            let me = tokio::task::try_id();
            if !task.is_finished() && me != Some(task.id()) {
                task.abort();
            }
        }
    }

    /// The channel closed (or its peer did): every request is cancelled.
    pub fn close(&self) {
        if self.inner.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let requests: Vec<Arc<LinkRequest>> = std::mem::take(&mut *self.inner.requests.lock()).into_values().collect();
        for req in &requests {
            Self::drop_request(req);
        }
        self.on_low(); // a drain waiting on the channel lets go
        self.inner.channel.close();
    }
}
