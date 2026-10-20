//! Stream a character-pack folder to the stick over BLE.
//!
//! Implements the char_begin → file → chunk → file_end → char_end protocol
//! from REFERENCE.md "Folder push". Strictly request/ack: we wait for each
//! ack before sending the next packet because the firmware's UART RX buffer
//! is only ~256 bytes and its base64 decode buffer is 300 bytes.
//!
//! Non-recursive, dotfiles skipped, 1.8 MB size cap per REFERENCE.md.
//! `manifest.json`'s `"name"` field (when present) overrides the folder
//! name for the `char_begin` message.
//!
//! Port of `eve_bridge/folder_push.py`. The daemon the Python passes in is the
//! [`PushTarget`] trait here; [`AckRouter`] is the port of the daemon's ack
//! waiters (`Daemon.expect_ack`, `Daemon.wait_for_ack` and the ack-routing
//! branch of `Daemon._handle_ble`), which exist to serve this protocol.

use std::ffi::{CStr, CString, OsStr};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::io::AsyncReadExt;
use tokio::sync::oneshot;

/// Raw bytes per chunk. Firmware's decode buffer is 300 bytes; we stay
/// comfortably below that so a single malformed byte can't overflow.
pub const CHUNK_SIZE: usize = 200;

/// Size cap per REFERENCE.md §Folder push.
pub const MAX_TOTAL_BYTES: u64 = 1_800_000;

/// Ack timeouts, in seconds. Flash erase on the stick can take ~200 ms for
/// large GIFs, so give each ack a generous window.
pub const ACK_TIMEOUT_FAST: f64 = 5.0; // file, chunk, file_end
/// char_begin (filesystem wipe), char_end (manifest parse + reload)
pub const ACK_TIMEOUT_SLOW: f64 = 15.0;

/// The two ack windows. Python reads the module constants at call time (the
/// tests monkeypatch them); Rust passes them to [`push_character_with`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AckTimeouts {
    /// file, chunk, file_end
    pub fast: Duration,
    /// char_begin, char_end
    pub slow: Duration,
}

impl Default for AckTimeouts {
    fn default() -> Self {
        Self {
            fast: Duration::from_secs_f64(ACK_TIMEOUT_FAST),
            slow: Duration::from_secs_f64(ACK_TIMEOUT_SLOW),
        }
    }
}

/// What `push_character` returns: Python's summary dict, keys in its order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PushSummary {
    pub name: String,
    pub files: usize,
    pub total_bytes: u64,
}

impl PushSummary {
    /// The summary as the JSON object Python returns (`{"name", "files", "total_bytes"}`).
    pub fn to_json(&self) -> Value {
        json!({"name": self.name, "files": self.files, "total_bytes": self.total_bytes})
    }
}

/// Why waiting for an ack ended without one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AckWaitError {
    /// `asyncio.wait_for` timed out.
    #[error("timed out")]
    Timeout,
    /// The waiter was dropped before an ack arrived (Python: the future was cancelled).
    #[error("cancelled")]
    Cancelled,
}

/// The errors Python's `push_character` raises. [`PushError::type_name`] is the
/// Python exception class name, so the daemon's IPC reply can keep Python's
/// `"{type(e).__name__}: {e}"` text.
#[derive(Debug, thiserror::Error)]
pub enum PushError {
    /// `ValueError`: not a directory, no files, folder too large.
    #[error("{0}")]
    Value(String),
    /// `RuntimeError`: BLE write failed, ack rejected.
    #[error("{0}")]
    Runtime(String),
    /// `TimeoutError` from `asyncio.wait_for`; its message is empty.
    #[error("")]
    Timeout,
    /// `CancelledError`: the ack waiter was cancelled under us.
    #[error("")]
    Cancelled,
    /// `AttributeError`: `manifest.json` parsed to something other than an object,
    /// so Python's `data.get("name")` raised.
    #[error("{0}")]
    Attribute(String),
    /// `OSError` (or its subclass) from listing, stat-ing or reading a file.
    #[error("{}", os_message(.errno, .strerror, .path))]
    Os {
        type_name: &'static str,
        errno: Option<i32>,
        strerror: String,
        path: Option<String>,
    },
}

impl PushError {
    /// The Python exception class name this error stands for.
    pub fn type_name(&self) -> &'static str {
        match self {
            PushError::Value(_) => "ValueError",
            PushError::Runtime(_) => "RuntimeError",
            PushError::Timeout => "TimeoutError",
            PushError::Cancelled => "CancelledError",
            PushError::Attribute(_) => "AttributeError",
            PushError::Os { type_name, .. } => type_name,
        }
    }

    fn os(err: &io::Error, path: Option<&Path>) -> Self {
        let errno = err.raw_os_error();
        let type_name = match errno {
            Some(libc::ENOENT) => "FileNotFoundError",
            Some(libc::EACCES) | Some(libc::EPERM) => "PermissionError",
            Some(libc::EISDIR) => "IsADirectoryError",
            Some(libc::ENOTDIR) => "NotADirectoryError",
            Some(libc::EEXIST) => "FileExistsError",
            Some(libc::EINTR) => "InterruptedError",
            _ => "OSError",
        };
        let strerror = match errno {
            Some(code) => strerror(code),
            None => err.to_string(),
        };
        PushError::Os {
            type_name,
            errno,
            strerror,
            path: path.map(|p| p.to_string_lossy().into_owned()),
        }
    }
}

impl From<AckWaitError> for PushError {
    fn from(e: AckWaitError) -> Self {
        match e {
            AckWaitError::Timeout => PushError::Timeout,
            AckWaitError::Cancelled => PushError::Cancelled,
        }
    }
}

/// `str(OSError)`: `[Errno 2] No such file or directory: '/x'`.
fn os_message(errno: &Option<i32>, strerror: &str, path: &Option<String>) -> String {
    match (errno, path) {
        (Some(e), Some(p)) => format!("[Errno {e}] {strerror}: {}", py_repr_str(p)),
        (Some(e), None) => format!("[Errno {e}] {strerror}"),
        (None, _) => strerror.to_string(),
    }
}

/// C `strerror`, the text Python puts in an `OSError`.
fn strerror(code: i32) -> String {
    let text = io::Error::from_raw_os_error(code).to_string();
    let suffix = format!(" (os error {code})");
    text.strip_suffix(&suffix).unwrap_or(&text).to_string()
}

/// The daemon side of a folder push: what Python's `push_character` uses of the
/// `daemon` it is handed (`expect_ack`, `ble.send`, `wait_for_ack`).
#[async_trait]
pub trait PushTarget: Send + Sync {
    /// The registered waiter. Dropping it cancels it.
    type Waiter: Send;

    /// Register a waiter for `ack_type` now, before the request goes out.
    /// `n`, when given, is the ack's expected `"n"` field.
    fn expect_ack(&self, ack_type: &str, n: Option<i64>) -> Self::Waiter;

    /// `daemon.ble.send(payload)`: write one line over BLE; false when the write failed.
    async fn send(&self, payload: &Value) -> bool;

    /// Block until the waiter's ack arrives, or `timeout` passes.
    async fn wait_for_ack(
        &self,
        ack_type: &str,
        timeout: Duration,
        waiter: Self::Waiter,
    ) -> Result<Value, AckWaitError>;
}

// ---- the daemon's ack waiters ---------------------------------------------

struct AckEntry {
    id: u64,
    ack_type: String,
    n: Option<i64>,
    tx: oneshot::Sender<Value>,
}

/// A waiter from [`AckRouter::expect_ack`]: pass it to [`AckRouter::wait_for_ack`].
/// Dropping it cancels it, as Python's `waiter.cancel()` does.
#[derive(Debug)]
pub struct AckWaiter {
    id: u64,
    rx: oneshot::Receiver<Value>,
}

/// Port of the daemon's `_ack_waiters` list with `expect_ack`, `wait_for_ack`
/// and the ack-routing branch of `_handle_ble` ([`AckRouter::route`]).
#[derive(Default)]
pub struct AckRouter {
    waiters: Mutex<Vec<AckEntry>>,
    next_id: AtomicU64,
}

impl AckRouter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a waiter for `ack_type` NOW, before the request goes out, and
    /// return it; pass it to `wait_for_ack`. Registering after the send left a
    /// window in which the board's ack reached the router with no waiter and
    /// was dropped (verification/Eve/Serial.lean, C). `n`, when given, is the
    /// ack's expected "n" field.
    pub fn expect_ack(&self, ack_type: &str, n: Option<i64>) -> AckWaiter {
        let (tx, rx) = oneshot::channel();
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut waiters = self.waiters.lock();
        // Waiters whose request never went out (the send failed) were cancelled
        // by their caller; prune them here.
        waiters.retain(|w| !w.tx.is_closed());
        waiters.push(AckEntry {
            id,
            ack_type: ack_type.to_string(),
            n,
            tx,
        });
        AckWaiter { id, rx }
    }

    /// Block until we receive an ack matching `ack_type`. Used by the
    /// folder-push flow — the firmware requires a per-chunk ack before we
    /// send the next chunk, since its UART RX buffer is only ~256 bytes.
    /// Pass the `waiter` from `expect_ack` to wait on one registered before
    /// the send; without it the waiter is registered here.
    pub async fn wait_for_ack(
        &self,
        ack_type: &str,
        timeout: Duration,
        waiter: Option<AckWaiter>,
    ) -> Result<Value, AckWaitError> {
        let waiter = match waiter {
            Some(w) => w,
            None => self.expect_ack(ack_type, None),
        };
        let id = waiter.id;
        let out = tokio::time::timeout(timeout, waiter.rx).await;
        self.waiters.lock().retain(|w| w.id != id);
        match out {
            Err(_) => Err(AckWaitError::Timeout),
            Ok(Err(_)) => Err(AckWaitError::Cancelled),
            Ok(Ok(v)) => Ok(v),
        }
    }

    /// Route an ack to the oldest waiter that registered for that ack type. A
    /// waiter that names an "n" takes only a successful ack carrying that n: a
    /// late ack from a timed-out request must not answer the next one
    /// (firmware failure acks carry n=0, so those still match). Returns
    /// whether a waiter took it. `obj` without a non-null `"ack"` is not an ack.
    pub fn route(&self, obj: &Value) -> bool {
        let ack = match obj.get("ack") {
            None | Some(Value::Null) => return false,
            Some(a) => a,
        };
        let Some(ack) = ack.as_str() else {
            return false;
        };
        let mut waiters = self.waiters.lock();
        let mut i = 0;
        while i < waiters.len() {
            let w = &waiters[i];
            if w.ack_type != ack || w.tx.is_closed() {
                i += 1;
                continue;
            }
            if let Some(want) = w.n {
                if py_truthy(obj.get("ok")) && !py_eq_int(obj.get("n"), want) {
                    tracing::debug!(
                        "ack {} n={} is stale (waiting for n={}) — ignored",
                        ack,
                        obj.get("n").map(py_repr_value).unwrap_or_else(|| "None".into()),
                        want
                    );
                    i += 1;
                    continue;
                }
            }
            let entry = waiters.remove(i);
            if entry.tx.send(obj.clone()).is_ok() {
                return true;
            }
            // The waiter went away between the check and the send: it is done,
            // so the next one is considered, as Python skips a done future.
        }
        false
    }

    /// How many waiters are registered (live or not yet pruned).
    pub fn pending(&self) -> usize {
        self.waiters.lock().len()
    }
}

// ---- folder walk ------------------------------------------------------------

/// Flat file listing, dotfiles skipped, deterministically ordered.
/// Order matters because the firmware writes files one at a time and some
/// might reference others (manifest.json conventionally first doesn't hurt).
pub fn enumerate_files(folder: &Path) -> Result<Vec<PathBuf>, PushError> {
    let entries = std::fs::read_dir(folder).map_err(|e| PushError::os(&e, Some(folder)))?;
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| PushError::os(&e, Some(folder)))?;
        let child = entry.path();
        // Path.is_file follows symlinks and answers False on any OSError.
        if !std::fs::metadata(&child).map(|m| m.is_file()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        files.push((name, child));
    }
    // manifest.json first, then the rest alphabetically — mirrors what the
    // desktop app seems to prefer.
    files.sort_by(|a, b| {
        let ka = (if a.0 == "manifest.json" { 0 } else { 1 }, &a.0);
        let kb = (if b.0 == "manifest.json" { 0 } else { 1 }, &b.0);
        ka.cmp(&kb)
    });
    Ok(files.into_iter().map(|(_, p)| p).collect())
}

/// `manifest.json`'s `name` field if valid; otherwise the folder's own
/// basename. Matches REFERENCE.md behaviour.
///
/// Like Python, a manifest that parses to something other than an object
/// raises (`AttributeError` on `data.get`).
pub fn pack_name(folder: &Path) -> Result<String, PushError> {
    let manifest = folder.join("manifest.json");
    let folder_name = path_name(folder);
    // Path.exists follows symlinks and answers False on any OSError.
    if std::fs::metadata(&manifest).is_err() {
        return Ok(folder_name);
    }
    let data: Value = match std::fs::read(&manifest)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
    {
        Some(v) => v,
        None => return Ok(folder_name),
    };
    let Value::Object(map) = &data else {
        return Err(PushError::Attribute(format!(
            "'{}' object has no attribute 'get'",
            py_type_name(&data)
        )));
    };
    if let Some(Value::String(name)) = map.get("name") {
        let stripped = py_strip(name);
        if !stripped.is_empty() {
            return Ok(stripped.to_string());
        }
    }
    Ok(folder_name)
}

/// Send `folder_path` to the connected stick. Returns a summary.
pub async fn push_character<D: PushTarget + ?Sized>(
    daemon: &D,
    folder_path: &str,
) -> Result<PushSummary, PushError> {
    push_character_with(daemon, folder_path, AckTimeouts::default()).await
}

/// [`push_character`] with explicit ack windows.
pub async fn push_character_with<D: PushTarget + ?Sized>(
    daemon: &D,
    folder_path: &str,
    timeouts: AckTimeouts,
) -> Result<PushSummary, PushError> {
    let folder = resolve(&expanduser(folder_path));
    if !std::fs::metadata(&folder).map(|m| m.is_dir()).unwrap_or(false) {
        return Err(PushError::Value(format!(
            "not a directory: {}",
            folder.display()
        )));
    }

    let files = enumerate_files(&folder)?;
    if files.is_empty() {
        return Err(PushError::Value(format!(
            "no files to push in {}",
            folder.display()
        )));
    }

    let mut total_bytes: u64 = 0;
    for f in &files {
        total_bytes += file_size(f)?;
    }
    if total_bytes > MAX_TOTAL_BYTES {
        return Err(PushError::Value(format!(
            "folder too large: {} bytes > {} cap",
            thousands(total_bytes),
            thousands(MAX_TOTAL_BYTES)
        )));
    }

    let name = pack_name(&folder)?;
    tracing::info!(
        "folder_push: name={}  files={}  total={} bytes",
        py_repr_str(&name),
        files.len(),
        total_bytes
    );

    send_expect(
        daemon,
        json!({"cmd": "char_begin", "name": name, "total": total_bytes}),
        "char_begin",
        timeouts.slow,
        None,
    )
    .await?;

    let mut bytes_pushed: u64 = 0;
    for fp in &files {
        let size = file_size(fp)?;
        let fname = path_name(fp);
        send_expect(
            daemon,
            json!({"cmd": "file", "path": fname, "size": size}),
            "file",
            timeouts.fast,
            None,
        )
        .await?;

        let mut file_bytes: i64 = 0;
        let mut fh = tokio::fs::File::open(fp)
            .await
            .map_err(|e| PushError::os(&e, Some(fp)))?;
        loop {
            let piece = read_piece(&mut fh).await?;
            if piece.is_empty() {
                break;
            }
            let b64 = base64::engine::general_purpose::STANDARD.encode(&piece);
            // The firmware's chunk ack carries n = bytes written to this
            // file so far (xfer.h), so this chunk's ack is exactly n=file_bytes.
            file_bytes += piece.len() as i64;
            send_expect(
                daemon,
                json!({"cmd": "chunk", "d": b64}),
                "chunk",
                timeouts.fast,
                Some(file_bytes),
            )
            .await?;
            bytes_pushed += piece.len() as u64;
        }

        send_expect(
            daemon,
            json!({"cmd": "file_end"}),
            "file_end",
            timeouts.fast,
            None,
        )
        .await?;
        tracing::info!(
            "folder_push: {} done ({}/{} bytes total)",
            fname,
            bytes_pushed,
            total_bytes
        );
    }

    send_expect(
        daemon,
        json!({"cmd": "char_end"}),
        "char_end",
        timeouts.slow,
        None,
    )
    .await?;
    tracing::info!(
        "folder_push: char_end acked — stick has switched to {}",
        py_repr_str(&name)
    );

    Ok(PushSummary {
        name,
        files: files.len(),
        total_bytes,
    })
}

/// Write one line over BLE and block until a matching ack arrives.
///
/// The waiter is registered BEFORE the line goes out: the board can answer
/// before this coroutine resumes from the send, and an ack with no waiter is
/// dropped (verification/Eve/Serial.lean, C). `n` is the ack's expected
/// "n" field, so a late ack from an earlier request cannot answer this one.
pub async fn send_expect<D: PushTarget + ?Sized>(
    daemon: &D,
    payload: Value,
    ack_type: &str,
    timeout: Duration,
    n: Option<i64>,
) -> Result<Value, PushError> {
    let waiter = daemon.expect_ack(ack_type, n);
    // On every early return below the waiter is dropped, which cancels it
    // (Python's `finally: waiter.cancel()`).
    if !daemon.send(&payload).await {
        let cmd = payload
            .get("cmd")
            .map(py_str_value)
            .unwrap_or_else(|| "None".to_string());
        return Err(PushError::Runtime(format!("ble write failed for cmd:{cmd}")));
    }
    let ack = daemon.wait_for_ack(ack_type, timeout, waiter).await?;
    if !py_truthy(ack.get("ok")) {
        let err = match ack.get("error") {
            Some(e) if py_truthy(Some(e)) => py_str_value(e),
            _ => "no detail".to_string(),
        };
        return Err(PushError::Runtime(format!("{ack_type} rejected: {err}")));
    }
    Ok(ack)
}

/// `fh.read(CHUNK_SIZE)` on a buffered binary file: up to CHUNK_SIZE bytes,
/// short only at end of file.
async fn read_piece(fh: &mut tokio::fs::File) -> Result<Vec<u8>, PushError> {
    let mut buf = vec![0u8; CHUNK_SIZE];
    let mut got = 0;
    while got < CHUNK_SIZE {
        match fh.read(&mut buf[got..]).await {
            Ok(0) => break,
            Ok(k) => got += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(PushError::os(&e, None)),
        }
    }
    buf.truncate(got);
    Ok(buf)
}

/// `Path.stat().st_size` (follows symlinks).
fn file_size(p: &Path) -> Result<u64, PushError> {
    std::fs::metadata(p)
        .map(|m| m.len())
        .map_err(|e| PushError::os(&e, Some(p)))
}

/// `Path.name`: the last component, "" for a root.
fn path_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `f"{n:,}"`.
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

// ---- Path.expanduser / Path.resolve ------------------------------------------

/// `os.path.expanduser` (POSIX): `~` is `$HOME` (or the passwd entry), `~user`
/// is that user's home; an unknown user leaves the path unchanged.
pub fn expanduser(path: &str) -> PathBuf {
    expanduser_with(path, std::env::var_os("HOME").map(PathBuf::from))
}

fn expanduser_with(path: &str, home_env: Option<PathBuf>) -> PathBuf {
    if !path.starts_with('~') {
        return PathBuf::from(path);
    }
    let i = path[1..].find('/').map(|k| k + 1).unwrap_or(path.len());
    let user = &path[1..i];
    let home: Option<Vec<u8>> = if user.is_empty() {
        match home_env {
            Some(h) => Some(h.as_os_str().as_bytes().to_vec()),
            None => passwd_home(None),
        }
    } else {
        passwd_home(Some(user))
    };
    let Some(home) = home else {
        return PathBuf::from(path);
    };
    let mut trimmed: &[u8] = &home;
    while let Some(rest) = trimmed.strip_suffix(b"/") {
        trimmed = rest;
    }
    let mut out = trimmed.to_vec();
    out.extend_from_slice(&path.as_bytes()[i..]);
    if out.is_empty() {
        return PathBuf::from("/");
    }
    PathBuf::from(OsStr::from_bytes(&out))
}

/// The passwd home directory of `user`, or of the current uid when `None`.
fn passwd_home(user: Option<&str>) -> Option<Vec<u8>> {
    let mut buf = vec![0 as libc::c_char; 16 * 1024];
    // SAFETY: zeroed passwd is a valid out-parameter for getpw*_r.
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    let rc = match user {
        Some(u) => {
            let cname = CString::new(u).ok()?;
            // SAFETY: all pointers are valid for the call; buf outlives it.
            unsafe {
                libc::getpwnam_r(
                    cname.as_ptr(),
                    &mut pwd,
                    buf.as_mut_ptr(),
                    buf.len(),
                    &mut result,
                )
            }
        }
        // SAFETY: as above.
        None => unsafe {
            libc::getpwuid_r(
                libc::getuid(),
                &mut pwd,
                buf.as_mut_ptr(),
                buf.len(),
                &mut result,
            )
        },
    };
    if rc != 0 || result.is_null() || pwd.pw_dir.is_null() {
        return None;
    }
    // SAFETY: pw_dir points into buf, NUL-terminated by getpw*_r.
    Some(unsafe { CStr::from_ptr(pwd.pw_dir) }.to_bytes().to_vec())
}

/// `Path.resolve()` (non-strict): absolute, symlinks of the existing part
/// resolved, `..` and `.` folded; a missing tail is kept as written.
pub fn resolve(path: &Path) -> PathBuf {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|c| c.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    if let Ok(real) = std::fs::canonicalize(&abs) {
        return real;
    }
    let mut out = PathBuf::from("/");
    let mut existing = true;
    for comp in abs.components() {
        match comp {
            Component::RootDir | Component::Prefix(_) | Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(part) => {
                out.push(part);
                if existing {
                    match std::fs::canonicalize(&out) {
                        Ok(real) => out = real,
                        Err(_) => {
                            // A dangling symlink resolves to its target, as realpath does.
                            if let Ok(target) = std::fs::read_link(&out) {
                                let base = out.parent().map(Path::to_path_buf).unwrap_or_default();
                                out = fold(&base.join(target));
                            }
                            existing = false;
                        }
                    }
                }
            }
        }
    }
    out
}

/// Lexically fold `.` and `..` in an absolute path.
fn fold(p: &Path) -> PathBuf {
    let mut out = PathBuf::from("/");
    for comp in p.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(part) => out.push(part),
            _ => {}
        }
    }
    out
}

// ---- Python value semantics -------------------------------------------------

/// Python truthiness of a JSON value (`None` for a missing key).
fn py_truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// Python `obj.get("n") == want` for an int `want` (`True == 1`, `200.0 == 200`).
fn py_eq_int(v: Option<&Value>, want: i64) -> bool {
    match v {
        Some(Value::Number(n)) => {
            if let Some(i) = n.as_i64() {
                i == want
            } else if let Some(u) = n.as_u64() {
                i64::try_from(u).map(|u| u == want).unwrap_or(false)
            } else {
                n.as_f64().map(|f| f == want as f64).unwrap_or(false)
            }
        }
        Some(Value::Bool(b)) => i64::from(*b) == want,
        _ => false,
    }
}

/// Python's type name for a parsed JSON value.
fn py_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// `str(value)` for a parsed JSON value.
fn py_str_value(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => py_repr_value(other),
    }
}

/// `repr(value)` for a parsed JSON value.
fn py_repr_value(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => {
            if n.is_f64() {
                let f = n.as_f64().unwrap_or(0.0);
                if f.is_finite() && f == f.trunc() && f.abs() < 1e16 {
                    format!("{f:.1}")
                } else {
                    format!("{f}")
                }
            } else {
                n.to_string()
            }
        }
        Value::String(s) => py_repr_str(s),
        Value::Array(a) => {
            let parts: Vec<String> = a.iter().map(py_repr_value).collect();
            format!("[{}]", parts.join(", "))
        }
        Value::Object(o) => {
            let parts: Vec<String> = o
                .iter()
                .map(|(k, v)| format!("{}: {}", py_repr_str(k), py_repr_value(v)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
    }
}

/// `repr(str)`: single quotes unless the text has a `'` and no `"`.
fn py_repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// `str.isspace` for one character: Unicode whitespace plus the ASCII
/// separators 0x1c–0x1f that Python also counts.
fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `str.strip()`.
fn py_strip(s: &str) -> &str {
    s.trim_matches(py_isspace)
}

#[cfg(test)]
mod tests;
