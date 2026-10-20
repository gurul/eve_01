//! The picture: ffmpeg's frames ([`ffmpeg_frames`], Python's `ObsbotPanel.frames`) and the one capture every viewer
//! shares ([`Capture`]).

use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::{Stream, StreamExt};
use parking_lot::Mutex;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, ChildStdout, Command};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::info;

use eve_obsbot::py;
use eve_pika::pika::split_jpegs;

use crate::{
    find_ffmpeg, DEVICE_RE, LINGER_SECS, MAX_VIDEO_BUFFER, NO_FFMPEG_LINE, VIDEO_DENIED_LINE, VIDEO_FIRST_SECS,
    VIDEO_FPS, VIDEO_FRAME_SECS, VIDEO_OFF_LINE, VIDEO_WIDTH,
};

/// Python's `LookupError` with the owner's line: the picture can't start, ended or went quiet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoError(pub String);

impl std::fmt::Display for VideoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for VideoError {}

impl VideoError {
    pub fn new(line: &str) -> VideoError {
        VideoError(line.to_string())
    }
}

/// One item of a frame source: a whole JPEG, or the line that ends it (Python raised LookupError).
pub type FrameResult = Result<Bytes, VideoError>;
/// A frame source (Python's async generator of JPEGs). It ends after an `Err`, or by ending.
pub type FrameStream = Pin<Box<dyn Stream<Item = FrameResult> + Send>>;
/// Makes a new frame source: one call per ffmpeg run.
pub type FramesFn = Arc<dyn Fn() -> FrameStream + Send + Sync>;

// ---- ffmpeg ----------------------------------------------------------------------------------------------------

/// The Tiny 3's AVFoundation index, by name: the list's order changes when cameras come and go. Python's `_device`.
pub(crate) async fn device(ffmpeg: &str) -> Option<String> {
    let out = Command::new(ffmpeg)
        .args(["-hide_banner", "-f", "avfoundation", "-list_devices", "true", "-i", ""])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await;
    let text = match out {
        Ok(out) => String::from_utf8_lossy(&out.stderr).into_owned(),
        Err(e) => {
            info!("obsbot: ffmpeg didn't run ({e})");
            return None;
        }
    };
    device_in(&text)
}

/// The listing half of [`device`]: the first OBSBOT line among the video devices.
pub(crate) fn device_in(text: &str) -> Option<String> {
    let lines = py::splitlines(text);
    for line in &lines {
        if line.contains("audio devices") {
            break;
        }
        if let Some(m) = DEVICE_RE.captures(line) {
            return Some(m[1].to_string());
        }
    }
    let tail = lines[lines.len().saturating_sub(6)..].join(" | ");
    info!("obsbot: no OBSBOT camera in ffmpeg's list ({})", last_chars(&tail, 300));
    None
}

/// Python's `s[-n:]`, in characters.
fn last_chars(s: &str, n: usize) -> &str {
    let count = s.chars().count();
    if count <= n {
        return s;
    }
    let (i, _) = s.char_indices().nth(count - n).unwrap_or((0, ' '));
    &s[i..]
}

struct Running {
    child: Child,
    stdout: ChildStdout,
    buf: Vec<u8>,
    first: bool,
}

enum State {
    Start(Option<String>),
    Running(Box<Running>),
    Done,
}

/// The webcam as whole JPEGs, the newest only; an `Err` with the owner's line when it can't start. Dropping the
/// stream kills ffmpeg (Python's `finally`).
pub(crate) fn ffmpeg_frames(ffmpeg: Option<String>) -> FrameStream {
    Box::pin(futures::stream::unfold(State::Start(ffmpeg), step))
}

async fn step(state: State) -> Option<(FrameResult, State)> {
    match state {
        State::Done => None,
        State::Running(r) => read(r).await,
        State::Start(ffmpeg) => {
            let Some(ffmpeg) = ffmpeg.filter(|f| !f.is_empty()).or_else(find_ffmpeg) else {
                return Some((Err(VideoError::new(NO_FFMPEG_LINE)), State::Done));
            };
            let Some(index) = device(&ffmpeg).await else {
                return Some((Err(VideoError::new(VIDEO_OFF_LINE)), State::Done));
            };
            let vf = format!("fps={VIDEO_FPS},scale={VIDEO_WIDTH}:-2");
            let input = format!("{index}:none");
            let spawned = Command::new(&ffmpeg)
                .args([
                    "-hide_banner", "-loglevel", "error", "-f", "avfoundation", "-pixel_format", "nv12",
                    "-framerate", "30", "-video_size", "1280x720", "-i", &input, "-vf", &vf, "-q:v", "7", "-f",
                    "mjpeg", "pipe:1",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn();
            let mut child = match spawned {
                Ok(c) => c,
                Err(e) => {
                    info!("obsbot: ffmpeg didn't start ({e})");
                    return Some((Err(VideoError::new(VIDEO_OFF_LINE)), State::Done));
                }
            };
            let Some(stdout) = child.stdout.take() else {
                end(child).await;
                return Some((Err(VideoError::new(VIDEO_OFF_LINE)), State::Done));
            };
            read(Box::new(Running { child, stdout, buf: Vec::new(), first: true })).await
        }
    }
}

async fn read(mut r: Box<Running>) -> Option<(FrameResult, State)> {
    let mut data = vec![0u8; 65536];
    loop {
        let secs = if r.first { VIDEO_FIRST_SECS } else { VIDEO_FRAME_SECS };
        let n = match tokio::time::timeout(Duration::from_secs_f64(secs), r.stdout.read(&mut data)).await {
            Err(_) => {
                end(r.child).await;
                if r.first {
                    info!("obsbot: no frame from ffmpeg in {VIDEO_FIRST_SECS:.0} s");
                    return Some((Err(VideoError::new(VIDEO_OFF_LINE)), State::Done));
                }
                return None;
            }
            Ok(Ok(n)) => n,
            Ok(Err(_)) => 0, // a read that fails is the end of the pipe
        };
        if n == 0 {
            if r.first {
                let mut raw = Vec::new();
                if let Some(mut stderr) = r.child.stderr.take() {
                    let _ = stderr.read_to_end(&mut raw).await;
                }
                let err = String::from_utf8_lossy(&raw).to_lowercase();
                info!("obsbot: ffmpeg ended before a frame ({})", last_chars(py::strip(&err), 160));
                end(r.child).await;
                let line = if err.contains("not authorized") || err.contains("permission") || err.contains("access") {
                    VIDEO_DENIED_LINE
                } else {
                    VIDEO_OFF_LINE
                };
                return Some((Err(VideoError::new(line)), State::Done));
            }
            end(r.child).await;
            return None;
        }
        r.buf.extend_from_slice(&data[..n]);
        let (jpegs, rest) = split_jpegs(&r.buf);
        r.buf = rest;
        if r.buf.len() > MAX_VIDEO_BUFFER {
            r.buf.clear();
        }
        if let Some(last) = jpegs.into_iter().next_back() {
            r.first = false;
            return Some((Ok(Bytes::from(last)), State::Running(r)));
        }
    }
}

/// Python's `finally`: kill ffmpeg if it still runs, and reap it.
async fn end(mut child: Child) {
    if let Ok(None) = child.try_wait() {
        let _ = child.start_kill();
    }
    let _ = child.wait().await;
}

// ---- the shared capture ------------------------------------------------------------------------------------------

struct CapState {
    jpeg: Option<Bytes>,
    seq: u64,
    error: Option<String>,
    viewers: usize,
    task: Option<JoinHandle<()>>,
    linger: Option<JoinHandle<()>>,
}

struct CapInner {
    frames: FramesFn,
    linger_secs: f64,
    state: Mutex<CapState>,
    changed: watch::Sender<u64>,
}

impl CapInner {
    fn running(s: &CapState) -> bool {
        s.task.as_ref().is_some_and(|t| !t.is_finished())
    }

    /// Python's `_publish`: wake every viewer waiting for a change.
    fn publish(&self) {
        self.changed.send_modify(|v| *v = v.wrapping_add(1));
    }
}

/// One ffmpeg for every viewer. It starts with the first viewer, keeps only the newest frame, and stops
/// LINGER_SECS after the last one leaves (a phone that reconnects reuses it). Viewers never end each other: on
/// 2026-10-11 a single-viewer rule had two open screens (the phone and a simulator) take the camera from each
/// other every 3 s. Clones share one capture.
#[derive(Clone)]
pub struct Capture {
    inner: Arc<CapInner>,
}

impl Capture {
    pub fn new(frames: FramesFn) -> Capture {
        Capture::with_linger(frames, LINGER_SECS)
    }

    /// As `new`, keeping ffmpeg `linger_secs` after the last viewer (the tests shorten Python's `LINGER_SECS`).
    pub fn with_linger(frames: FramesFn, linger_secs: f64) -> Capture {
        Capture {
            inner: Arc::new(CapInner {
                frames,
                linger_secs,
                state: Mutex::new(CapState { jpeg: None, seq: 0, error: None, viewers: 0, task: None, linger: None }),
                changed: watch::channel(0).0,
            }),
        }
    }

    /// ffmpeg is running.
    pub fn running(&self) -> bool {
        CapInner::running(&self.inner.state.lock())
    }

    pub fn viewers(&self) -> usize {
        self.inner.state.lock().viewers
    }

    pub fn jpeg(&self) -> Option<Bytes> {
        self.inner.state.lock().jpeg.clone()
    }

    pub fn seq(&self) -> u64 {
        self.inner.state.lock().seq
    }

    pub fn error(&self) -> Option<String> {
        self.inner.state.lock().error.clone()
    }

    /// The newest frame each time there is a new one ([`Viewer::next`]). Like Python's generator, it joins the
    /// capture at its first `next`, not here.
    pub fn watch(&self) -> Viewer {
        Viewer { inner: self.inner.clone(), rx: None, seen: 0, first: true, started: false, finished: false }
    }

    pub async fn stop(&self) {
        stop(&self.inner).await;
    }
}

/// Ends the run (its guard sets the error and wakes the viewers), then forgets the frame.
async fn stop(inner: &Arc<CapInner>) {
    let task = inner.state.lock().task.take();
    if let Some(task) = task {
        if !task.is_finished() {
            task.abort(); // dropping the frame source kills ffmpeg
        }
        let _ = task.await;
    }
    inner.state.lock().jpeg = None;
}

/// Python's `_run`'s `finally`: the run ended (or was cancelled); viewers get its line.
struct RunEnd {
    inner: Arc<CapInner>,
    line: String,
}

impl Drop for RunEnd {
    fn drop(&mut self) {
        {
            let mut s = self.inner.state.lock();
            s.jpeg = None;
            s.error = Some(std::mem::take(&mut self.line));
        }
        self.inner.publish();
    }
}

async fn run(inner: Arc<CapInner>) {
    let mut end = RunEnd { inner: inner.clone(), line: VIDEO_OFF_LINE.to_string() };
    let mut frames = (inner.frames)();
    while let Some(item) = frames.next().await {
        match item {
            Ok(jpeg) => {
                {
                    let mut s = inner.state.lock();
                    s.jpeg = Some(jpeg);
                    s.seq += 1;
                    s.error = None;
                }
                inner.publish();
            }
            Err(e) => {
                end.line = e.0;
                break;
            }
        }
    }
    drop(frames);
    drop(end);
}

async fn stop_later(inner: Arc<CapInner>) {
    tokio::time::sleep(Duration::from_secs_f64(inner.linger_secs)).await;
    if inner.state.lock().viewers == 0 {
        stop(&inner).await;
    }
}

/// One viewer of a [`Capture`] (Python's `watch()` generator). Dropping it, or [`Viewer::close`], leaves.
pub struct Viewer {
    inner: Arc<CapInner>,
    rx: Option<watch::Receiver<u64>>,
    seen: u64,
    first: bool,
    started: bool,
    finished: bool,
}

impl Viewer {
    /// The next new frame; `Some(Err)` with the owner's line when the camera can't start, ends or goes quiet, and
    /// `None` after that.
    pub async fn next(&mut self) -> Option<FrameResult> {
        if self.finished {
            return None;
        }
        if !self.started {
            self.start();
        }
        loop {
            let secs = if self.first { VIDEO_FIRST_SECS } else { VIDEO_FRAME_SECS };
            if let Some(rx) = self.rx.as_mut() {
                rx.borrow_and_update(); // a change after this wakes the wait below
            }
            {
                let s = self.inner.state.lock();
                if s.seq != self.seen {
                    if let Some(jpeg) = s.jpeg.clone() {
                        self.seen = s.seq;
                        self.first = false;
                        return Some(Ok(jpeg));
                    }
                }
                if let Some(e) = s.error.clone() {
                    drop(s);
                    self.finish();
                    return Some(Err(VideoError(e)));
                }
            }
            let changed = match self.rx.as_mut() {
                Some(rx) => tokio::time::timeout(Duration::from_secs_f64(secs), rx.changed())
                    .await
                    .is_ok_and(|r| r.is_ok()),
                None => false,
            };
            if !changed {
                self.finish();
                return Some(Err(VideoError::new(VIDEO_OFF_LINE)));
            }
        }
    }

    fn start(&mut self) {
        self.started = true;
        let inner = self.inner.clone();
        let mut s = inner.state.lock();
        s.viewers += 1;
        if let Some(linger) = s.linger.take() {
            linger.abort();
        }
        if !CapInner::running(&s) {
            s.jpeg = None;
            s.error = None;
            s.task = Some(tokio::spawn(run(inner.clone())));
        }
        self.seen = if s.jpeg.is_some() { s.seq.wrapping_sub(1) } else { s.seq };
        self.rx = Some(inner.changed.subscribe());
    }

    /// Python's `finally`: one viewer fewer; the last one starts the linger.
    fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        if !self.started {
            return;
        }
        let mut s = self.inner.state.lock();
        s.viewers -= 1;
        if s.viewers == 0 && CapInner::running(&s) {
            if let Ok(rt) = tokio::runtime::Handle::try_current() {
                s.linger = Some(rt.spawn(stop_later(self.inner.clone())));
            }
        }
    }

    /// Python's `aclose()`.
    pub fn close(&mut self) {
        self.finish();
    }
}

impl Drop for Viewer {
    fn drop(&mut self) {
        self.finish();
    }
}
