//! Who may use eve lite's web app: the Mac's own browser and each paired phone or laptop (docs/plans/eve-lite.md M4).
//! Port of `eve_bridge/web_auth.py`.
//!
//! * **Cookies.** `eve_mac` on the Mac's own page and `eve_dev` on the LAN door. A cookie is scoped by host, not
//!   by port, so the two listeners need two names. Both are HttpOnly and SameSite=Strict and last 400 days (MAX_AGE,
//!   the most a browser keeps one). Only `eve_dev` is Secure: the Mac's page is plain http on loopback, where not
//!   every browser keeps a Secure cookie.
//! * **Devices** (`web-devices.json`, 0600). A cookie's value is a 256-bit token. The file keeps only its SHA-256,
//!   with a label, the host it was set on, the door it paired through (`local` for the Mac's own page, `lan` for a
//!   phone or laptop), and when the device paired and was last seen. `check` is told which door is asking and refuses
//!   a token from the other one, so a phone's `eve_dev` token cannot stand in as the Mac page's `eve_mac` and reach
//!   a Mac-only route. A row with no door reads as `lan` (the lower-privilege door). Revoking returns the ids, so the
//!   daemon can close those devices' live peers. A device unseen for MAX_AGE is forgotten: its cookie is gone too.
//! * **The pairing window.** Only the signed-in Mac page opens it. It holds one 128-bit secret, which travels in the
//!   QR code's `#p=` fragment (a browser never sends a fragment to a server), and one 6-digit code to type on a
//!   laptop. It lasts 5 minutes and pairs one device. 5 wrong tries close it, and a new window replaces the old one.
//! * **The Mac login.** `eve-bridge web open` asks the daemon, over its 0600 socket, for a one-time 128-bit code
//!   that lives 2 minutes, and hands it to the browser in a 0600 file: never on the screen, in argv or in a log.
//!
//! Secrets are compared in constant time, never logged, and never kept: tokens are hashed on disk, codes in memory.

use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use indexmap::IndexMap;
use parking_lot::Mutex;
use rand::{Rng, RngCore};
use regex::Regex;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use unicode_general_category::{get_general_category, GeneralCategory};

pub const COOKIE_MAC: &str = "eve_mac";
pub const COOKIE_DEV: &str = "eve_dev";
/// 400 days: browsers keep a cookie no longer (Chrome caps Max-Age there).
pub const MAX_AGE: i64 = 34_560_000;

pub const PAIR_SECS: f64 = 300.0;
/// Wrong tries that close a pairing window.
pub const PAIR_TRIES: u32 = 5;
pub const LOGIN_SECS: f64 = 120.0;
/// A used login code still answers `used()` this long after it expired.
pub const LOGIN_KEEP_SECS: f64 = 60.0;
/// Login codes waiting at once; past this the oldest goes.
pub const MAX_LOGINS: usize = 16;
/// Paired browsers; past this the one seen longest ago is dropped.
pub const MAX_DEVICES: usize = 64;
/// A device's last_seen reaches the file at most this often.
pub const SEEN_SECS: i64 = 600;
/// A stored label or host, in characters.
pub const TEXT_CHARS: usize = 64;

/// The Mac's own page, and the LAN door a phone or laptop pairs on.
pub const DOORS: [&str; 2] = ["local", "lan"];
/// A row with no door (an older file) reads as the lower-privilege one.
pub const DEFAULT_DOOR: &str = "lan";

/// What `secrets.token_urlsafe` makes; cookie-safe as it is. Anchored: `is_match` is Python's `fullmatch`.
pub static TOKEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\A[A-Za-z0-9_-]{16,256}\z").unwrap());
/// A device id. Anchored: `is_match` is Python's `fullmatch`.
pub static DEVICE_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\A[0-9a-f]{8}\z").unwrap());
/// A SHA-256 hex digest. Anchored: `is_match` is Python's `fullmatch`.
pub static SHA256_HEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\A[0-9a-f]{64}\z").unwrap());
/// "123 456" and "123-456" are the code typed with a gap. Python's `\s` also covers U+001C..U+001F.
pub static CODE_SEPARATORS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\s\x1c-\x1f-]").unwrap());
static SIX_DIGITS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\A[0-9]{6}\z").unwrap());

/// `web_auth.TOKEN.fullmatch(value)`.
pub fn is_token(value: &str) -> bool {
    TOKEN.is_match(value)
}

/// `web_auth.DEVICE_ID.fullmatch(value)`.
pub fn is_device_id(value: &str) -> bool {
    DEVICE_ID.is_match(value)
}

/// A clock in seconds, as `time.time` / `time.monotonic` give it. Tests pass a fake one.
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

/// `time.time`: seconds since the epoch.
pub fn wall_clock() -> Clock {
    Arc::new(|| SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0))
}

static MONOTONIC_START: LazyLock<Instant> = LazyLock::new(Instant::now);

/// `time.monotonic`: seconds that never go back (counted from this process's first use).
pub fn monotonic_clock() -> Clock {
    Arc::new(|| MONOTONIC_START.elapsed().as_secs_f64())
}

/// Python's `ValueError` from the cookie helpers.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CookieError {
    #[error("not an eve lite cookie")]
    NotACookie,
    #[error("not a device token")]
    NotAToken,
}

// ---- cookies --------------------------------------------------------------------------------------------

fn flags(name: &str) -> Result<&'static str, CookieError> {
    if name == COOKIE_DEV {
        return Ok("HttpOnly; Secure; SameSite=Strict; Path=/");
    }
    if name == COOKIE_MAC {
        return Ok("HttpOnly; SameSite=Strict; Path=/");
    }
    Err(CookieError::NotACookie)
}

/// The Set-Cookie value that gives a browser its device token, with the exact flags for its listener.
pub fn set_cookie(name: &str, token: &str) -> Result<String, CookieError> {
    let flags = flags(name)?;
    if !TOKEN.is_match(token) {
        return Err(CookieError::NotAToken);
    }
    Ok(format!("{name}={token}; {flags}; Max-Age={MAX_AGE}"))
}

/// The Set-Cookie value that removes the cookie (signing out).
pub fn clear_cookie(name: &str) -> Result<String, CookieError> {
    Ok(format!("{name}=; {}; Max-Age=0", flags(name)?))
}

/// Python's `str.strip()` set: `char::is_whitespace` plus U+001C..U+001F, which `str.isspace` also counts.
fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// The value of cookie `name` in a request's Cookie header, or "". A malformed pair is skipped, never
/// fatal (`http.cookies` drops every cookie after one it cannot parse).
pub fn cookie_value(header: Option<&str>, name: &str) -> String {
    for part in header.unwrap_or("").split(';') {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        if key.trim_matches(py_isspace) != name {
            continue;
        }
        let mut value = value.trim_matches(py_isspace);
        if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
            value = &value[1..value.len() - 1];
        }
        if !value.is_empty() {
            return value.to_string();
        }
    }
    String::new()
}

// ---- devices --------------------------------------------------------------------------------------------

/// One paired browser. Serialises as `Devices.list()` rows: {id, label, host, created, last_seen}.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Device {
    pub id: String,
    pub label: String,
    pub host: String,
    pub created: i64,
    pub last_seen: i64,
}

fn digest(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

/// `hmac.compare_digest`: the time depends on the length, never on where the bytes differ.
fn compare_digest(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// `unicodedata.category(c)[0] == "C"`: control, format, surrogate, private use, unassigned.
fn is_other(c: char) -> bool {
    matches!(
        get_general_category(c),
        GeneralCategory::Control
            | GeneralCategory::Format
            | GeneralCategory::Surrogate
            | GeneralCategory::PrivateUse
            | GeneralCategory::Unassigned
    )
}

/// A label or host as it may be stored and shown: one line, no control or format characters, TEXT_CHARS.
/// `None` stands for anything that is not a string.
fn text(value: Option<&str>) -> String {
    let Some(value) = value else {
        return String::new();
    };
    let kept: String = value
        .chars()
        .filter(|&c| py_isspace(c) || !is_other(c))
        .map(|c| if py_isspace(c) { ' ' } else { c })
        .collect();
    let joined = kept.split(' ').filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ");
    joined.chars().take(TEXT_CHARS).collect()
}

fn stamp(value: Option<&Value>) -> Option<i64> {
    // A JSON integer only: a bool or a float is not one, as in Python's isinstance(value, int) less bool.
    value.and_then(Value::as_i64).filter(|v| *v >= 0)
}

/// The door a row paired through, as it is stored: `local` or `lan`; anything else (an older row with no
/// door, a bad value) reads as `lan`, the lower-privilege door, so a hand-written one cannot claim the Mac's.
fn door(value: Option<&str>) -> String {
    match value {
        Some(v) if DOORS.contains(&v) => v.to_string(),
        _ => DEFAULT_DOOR.to_string(),
    }
}

/// One device as the file holds it. Field order is the file's key order.
#[derive(Debug, Clone, Serialize)]
struct Row {
    id: String,
    label: String,
    host: String,
    door: String,
    sha256: String,
    created: i64,
    last_seen: i64,
}

impl Row {
    fn device(&self) -> Device {
        Device {
            id: self.id.clone(),
            label: self.label.clone(),
            host: self.host.clone(),
            created: self.created,
            last_seen: self.last_seen,
        }
    }
}

/// One device as the file holds it, checked; None for anything else (dropped, never trusted).
fn row(raw: &Value) -> Option<Row> {
    let raw = raw.as_object()?;
    let dev_id = raw.get("id").and_then(Value::as_str)?;
    let digest = raw.get("sha256").and_then(Value::as_str)?;
    let created = stamp(raw.get("created"))?;
    let seen = stamp(raw.get("last_seen"))?;
    if !DEVICE_ID.is_match(dev_id) || !SHA256_HEX.is_match(digest) {
        return None;
    }
    Some(Row {
        id: dev_id.to_string(),
        label: text(raw.get("label").and_then(Value::as_str)),
        host: text(raw.get("host").and_then(Value::as_str)),
        door: door(raw.get("door").and_then(Value::as_str)),
        sha256: digest.to_string(),
        created,
        last_seen: seen,
    })
}

/// Python's exception class name for an OSError, for the log lines that name only the type.
fn os_error_name(e: &io::Error) -> &'static str {
    match e.raw_os_error() {
        Some(libc::ENOENT) => "FileNotFoundError",
        Some(libc::EACCES) | Some(libc::EPERM) => "PermissionError",
        Some(libc::EEXIST) => "FileExistsError",
        Some(libc::EISDIR) => "IsADirectoryError",
        Some(libc::ENOTDIR) => "NotADirectoryError",
        Some(libc::EINTR) => "InterruptedError",
        Some(libc::ETIMEDOUT) => "TimeoutError",
        _ => "OSError",
    }
}

/// Write `data` to `path` atomically and owner-only (0600), whatever the umask.
pub fn write_private(path: &Path, data: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.tmp"));
    match fs::remove_file(&tmp) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(&tmp)?;
    let result = (|| -> io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(data)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// `json.dumps(..., indent=2)`: serde's pretty form with Python's `ensure_ascii` escapes.
fn python_json(value: &Value) -> String {
    let pretty = serde_json::to_string_pretty(value).unwrap_or_default();
    let mut out = String::with_capacity(pretty.len());
    for c in pretty.chars() {
        if (c as u32) < 0x7f {
            out.push(c);
            continue;
        }
        let mut units = [0u16; 2];
        for unit in c.encode_utf16(&mut units) {
            out.push_str(&format!("\\u{unit:04x}"));
        }
    }
    out
}

/// What saves the device list; `write_private` unless a test swaps it.
pub type Writer = Arc<dyn Fn(&Path, &[u8]) -> io::Result<()> + Send + Sync>;

type FileStamp = (u64, u64, i128);

#[derive(Default)]
struct State {
    rows: Vec<Row>,
    /// (inode, size, mtime) when last read or written.
    file: Option<FileStamp>,
    read_once: bool,
}

/// The paired browsers, in `path` (`~/.config/eve-bridge/web-devices.json`, 0600), tokens as SHA-256 only.
/// Reads the file again whenever it changed on disk, so a revoke by another process counts at once. Thread-safe.
pub struct Devices {
    pub path: PathBuf,
    clock: Clock,
    writer: Writer,
    max_devices: usize,
    state: Mutex<State>,
}

impl fmt::Debug for Devices {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Devices").field("path", &self.path).finish_non_exhaustive()
    }
}

impl Devices {
    /// `Devices(path)`, on the wall clock.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self::with_clock(path, wall_clock())
    }

    /// `Devices(path, clock=clock)`.
    pub fn with_clock(path: impl Into<PathBuf>, clock: Clock) -> Self {
        Devices {
            path: path.into(),
            clock,
            writer: Arc::new(|p: &Path, d: &[u8]| write_private(p, d)),
            max_devices: MAX_DEVICES,
            state: Mutex::new(State::default()),
        }
    }

    /// Swap what saves the file (a test's full disk).
    pub fn with_writer(mut self, writer: Writer) -> Self {
        self.writer = writer;
        self
    }

    /// Keep at most `max` devices instead of MAX_DEVICES.
    pub fn with_max_devices(mut self, max: usize) -> Self {
        self.max_devices = max;
        self
    }

    fn now(&self) -> i64 {
        (self.clock)() as i64
    }

    /// Pair a browser on `door` (`local` for the Mac's own page, `lan` for a phone or laptop; DEFAULT_DOOR is the
    /// Python default). Returns its id and the token for its cookie: the token is never stored or shown, only its
    /// hash is kept. `host` is None when there is none. Errors when the list cannot be saved, and then nothing was
    /// paired.
    pub fn add(&self, label: &str, host: Option<&str>, door_: &str) -> io::Result<(String, String)> {
        let token = token_urlsafe(32);
        let (paired, dev_id) = {
            let mut st = self.state.lock();
            self.sync(&mut st);
            let now = self.now();
            let mut dev_id = token_hex(4);
            while st.rows.iter().any(|r| r.id == dev_id) {
                dev_id = token_hex(4);
            }
            let new = Row {
                id: dev_id.clone(),
                label: text(Some(label)),
                host: text(host),
                door: door(Some(door_)),
                sha256: digest(&token),
                created: now,
                last_seen: now,
            };
            let mut all = st.rows.clone();
            all.push(new);
            let rows = self.kept(all, now);
            self.write(&mut st, &rows)?;
            st.rows = rows;
            (st.rows.len(), dev_id)
        };
        tracing::info!("web auth: paired a device ({paired} paired)");
        Ok((dev_id, token))
    }

    /// The device a cookie's token belongs to, or None: unknown, revoked, malformed, unseen for MAX_AGE, or —
    /// when `door` is given — paired through the other door. The door is the caller's listener (`local` or
    /// `lan`): a phone's `eve_dev` token presented as the Mac page's `eve_mac` is refused here, so it reaches
    /// no Mac-only route. Marks the device seen.
    pub fn check(&self, token: &str, door: Option<&str>) -> Option<Device> {
        if !TOKEN.is_match(token) {
            return None;
        }
        let digest = digest(token);
        let mut st = self.state.lock();
        self.sync(&mut st);
        let now = self.now();
        let mut found: Option<usize> = None;
        for (i, row) in st.rows.iter().enumerate() {
            // every row, each in constant time
            if compare_digest(row.sha256.as_bytes(), digest.as_bytes()) {
                found = Some(i);
            }
        }
        let i = found?;
        if now - st.rows[i].last_seen > MAX_AGE || door.is_some_and(|d| st.rows[i].door != d) {
            return None;
        }
        if now - st.rows[i].last_seen >= SEEN_SECS {
            st.rows[i].last_seen = now; // in memory even when the save below fails, as in Python
            let device = st.rows[i].device();
            let rows = self.kept(st.rows.clone(), now);
            match self.write(&mut st, &rows) {
                Ok(()) => st.rows = rows,
                // a full disk must not sign anyone out
                Err(e) => tracing::warn!(
                    "web auth: could not save when a device was last seen ({})",
                    os_error_name(&e)
                ),
            }
            return Some(device);
        }
        Some(st.rows[i].device())
    }

    /// Forget one device by its id, or every device with "all". Returns the ids revoked, so their live
    /// connections can be closed; [] when nothing matched. The tokens stop working at once; an error when the list
    /// cannot be saved, because then the revoke would not outlive a restart.
    pub fn revoke(&self, id_or_all: &str) -> io::Result<Vec<String>> {
        let want = id_or_all.trim_matches(py_isspace).to_lowercase();
        let mut st = self.state.lock();
        self.sync(&mut st);
        let gone: Vec<String> = st
            .rows
            .iter()
            .filter(|r| !want.is_empty() && (want == "all" || r.id == want))
            .map(|r| r.id.clone())
            .collect();
        if !gone.is_empty() {
            st.rows.retain(|r| !gone.contains(&r.id));
            tracing::info!("web auth: revoked {} device(s)", gone.len());
            let now = self.now();
            let rows = self.kept(st.rows.clone(), now);
            self.write(&mut st, &rows)?;
        }
        Ok(gone)
    }

    /// Every paired device, oldest first, as {id, label, host, created, last_seen}. Never the hash.
    pub fn list(&self) -> Vec<Device> {
        let mut rows: Vec<Device> = {
            let mut st = self.state.lock();
            self.sync(&mut st);
            let now = self.now();
            st.rows.iter().filter(|r| now - r.last_seen <= MAX_AGE).map(Row::device).collect()
        };
        rows.sort_by(|a, b| (a.created, &a.id).cmp(&(b.created, &b.id)));
        rows
    }

    // ---- the file ------------------------------------------------------------------------------------------

    fn stat(&self) -> Option<FileStamp> {
        let meta = fs::metadata(&self.path).ok()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Some((meta.ino(), meta.size(), meta.mtime() as i128 * 1_000_000_000 + meta.mtime_nsec() as i128))
        }
        #[cfg(not(unix))]
        {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as i128)
                .unwrap_or(0);
            Some((0, meta.len(), mtime))
        }
    }

    /// Read the file when this object has not yet, or it changed since this object last read or wrote it.
    fn sync(&self, st: &mut State) {
        let now_file = self.stat();
        if st.read_once && now_file == st.file {
            return;
        }
        st.rows = self.read();
        st.file = now_file;
        st.read_once = true;
    }

    fn read(&self) -> Vec<Row> {
        let bytes = match fs::read(&self.path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Vec::new(),
            Err(e) => return unreadable(os_error_name(&e)),
        };
        let Ok(text) = String::from_utf8(bytes) else {
            return unreadable("UnicodeDecodeError");
        };
        let data: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => return unreadable("JSONDecodeError"),
        };
        let raw = data.as_object().and_then(|d| d.get("devices")).and_then(Value::as_array);
        let mut unique: IndexMap<String, Row> = IndexMap::new();
        for r in raw.into_iter().flatten().filter_map(row) {
            unique.entry(r.id.clone()).or_insert(r);
        }
        unique.into_values().collect()
    }

    /// `rows` less the devices unseen for MAX_AGE and, past the cap, the ones seen longest ago.
    fn kept(&self, rows: Vec<Row>, now: i64) -> Vec<Row> {
        let rows: Vec<Row> = rows.into_iter().filter(|r| now - r.last_seen <= MAX_AGE).collect();
        if rows.len() <= self.max_devices {
            return rows;
        }
        let mut oldest: Vec<&Row> = rows.iter().collect();
        oldest.sort_by_key(|r| (r.last_seen, r.created)); // stable, as Python's sorted
        let dropped: Vec<String> = oldest[..rows.len() - self.max_devices].iter().map(|r| r.id.clone()).collect();
        let rows: Vec<Row> = rows.into_iter().filter(|r| !dropped.contains(&r.id)).collect();
        tracing::info!(
            "web auth: forgot {} device(s) seen longest ago (more than {} paired)",
            dropped.len(),
            self.max_devices
        );
        rows
    }

    fn write(&self, st: &mut State, rows: &[Row]) -> io::Result<()> {
        let body = python_json(&serde_json::json!({"version": 1, "devices": rows})) + "\n";
        (self.writer)(&self.path, body.as_bytes())?;
        st.file = self.stat();
        st.read_once = true;
        Ok(())
    }
}

fn unreadable(kind: &str) -> Vec<Row> {
    tracing::warn!("web auth: the device list could not be read ({kind}); every device pairs again");
    Vec::new()
}

/// `secrets.token_urlsafe(n)`: n random bytes, URL-safe base64 without padding.
fn token_urlsafe(n: usize) -> String {
    let mut bytes = vec![0u8; n];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// `secrets.token_hex(n)`.
fn token_hex(n: usize) -> String {
    let mut bytes = vec![0u8; n];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

// ---- pairing a phone or laptop --------------------------------------------------------------------------

/// A 6-digit code as typed ("123456", "123 456", "123-456", or the number 123456), or None.
fn code_digits(code: &Value) -> Option<String> {
    match code {
        Value::Number(n) => {
            // an integer only: 123456.0 is a float in Python too
            let v = n.as_i64()?;
            (0..1_000_000).contains(&v).then(|| format!("{v:06}"))
        }
        Value::String(s) if s.chars().count() <= 32 => {
            let digits = CODE_SEPARATORS.replace_all(s, "");
            SIX_DIGITS.is_match(&digits).then(|| digits.into_owned())
        }
        _ => None,
    }
}

#[derive(Default)]
struct Window {
    /// SHA-256 of the secret and of the code, never the values.
    secret: Vec<u8>,
    code: Vec<u8>,
    until: f64,
    tries: u32,
    open: bool,
}

impl Window {
    fn shut(&mut self) {
        self.open = false;
        self.secret.clear();
        self.code.clear();
    }

    fn live(&mut self, now: f64) -> bool {
        if self.open && now >= self.until {
            self.shut();
        }
        self.open
    }
}

/// At most one open pairing at a time: a 128-bit secret for the QR code and a 6-digit code to type. Either
/// pairs one device, once, before `ttl` runs out; PAIR_TRIES wrong tries close the window first. Thread-safe.
pub struct PairingWindow {
    clock: Clock,
    inner: Mutex<Window>,
}

impl fmt::Debug for PairingWindow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairingWindow").field("open", &self.inner.lock().open).finish_non_exhaustive()
    }
}

impl Default for PairingWindow {
    fn default() -> Self {
        Self::new()
    }
}

impl PairingWindow {
    /// `PairingWindow()`, on the monotonic clock.
    pub fn new() -> Self {
        Self::with_clock(monotonic_clock())
    }

    pub fn with_clock(clock: Clock) -> Self {
        PairingWindow { clock, inner: Mutex::new(Window::default()) }
    }

    /// A new window of PAIR_SECS, replacing any open one.
    pub fn open(&self) -> (String, String) {
        self.open_for(PAIR_SECS)
    }

    /// A new window, replacing any open one: (secret for `#p=`, 6-digit code). Only the signed-in Mac page
    /// may call this.
    pub fn open_for(&self, ttl: f64) -> (String, String) {
        let secret = token_urlsafe(16);
        let code = format!("{:06}", rand::rng().random_range(0..1_000_000u32));
        {
            let mut w = self.inner.lock();
            w.secret = Sha256::digest(secret.as_bytes()).to_vec();
            w.code = Sha256::digest(code.as_bytes()).to_vec();
            w.until = (self.clock)() + ttl.max(0.0);
            w.tries = 0;
            w.open = true;
        }
        tracing::info!("web auth: pairing window open for {} s", ttl as i64);
        (secret, code)
    }

    /// Close the window now: the Mac page cancelled it, or the LAN door turned off.
    pub fn close(&self) {
        self.inner.lock().shut();
    }

    pub fn is_open(&self) -> bool {
        let now = (self.clock)();
        self.inner.lock().live(now)
    }

    /// Seconds until the window closes by itself, 0.0 when it is closed.
    pub fn remaining(&self) -> f64 {
        let now = (self.clock)();
        let mut w = self.inner.lock();
        if w.live(now) {
            (w.until - now).max(0.0)
        } else {
            0.0
        }
    }

    /// True once: for the window's secret or its code, given alone, while the window is open. Any other try
    /// counts as wrong (both given, neither, malformed, not a match), and the PAIR_TRIES-th closes the window.
    /// The arguments are the request body's `secret` and `code` as they came; JSON null is the same as absent.
    pub fn pair(&self, secret: Option<&Value>, code: Option<&Value>) -> bool {
        let secret = secret.filter(|v| !v.is_null());
        let code = code.filter(|v| !v.is_null());
        let mut w = self.inner.lock();
        if !w.live((self.clock)()) {
            return false;
        }
        let mut given: Option<Vec<u8>> = None;
        let mut want: &[u8] = b"";
        match (secret, code) {
            (Some(s), None) => {
                if let Some(s) = s.as_str().filter(|s| s.chars().count() <= 64) {
                    given = Some(Sha256::digest(s.as_bytes()).to_vec());
                    want = &w.secret;
                }
            }
            (None, Some(c)) => {
                if let Some(digits) = code_digits(c) {
                    given = Some(Sha256::digest(digits.as_bytes()).to_vec());
                    want = &w.code;
                }
            }
            _ => {}
        }
        if given.as_deref().is_some_and(|g| compare_digest(g, want)) {
            w.shut();
            drop(w);
            tracing::info!(
                "web auth: a device paired through the window ({})",
                if secret.is_some() { "secret" } else { "code" }
            );
            return true;
        }
        w.tries += 1;
        if w.tries >= PAIR_TRIES {
            w.shut();
            tracing::warn!("web auth: pairing window closed after {} wrong tries", w.tries);
        }
        false
    }
}

// ---- the Mac's own browser ------------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Login {
    until: f64,
    keep: f64,
    used: bool,
}

/// One-time codes that sign the Mac's own browser in. Each works once, before its `ttl`; only its hash is
/// kept. Thread-safe.
pub struct MacLogin {
    clock: Clock,
    /// SHA-256 hex of each code -> its life, oldest first.
    codes: Mutex<IndexMap<String, Login>>,
}

impl fmt::Debug for MacLogin {
    /// Shows what is held: the codes' hashes, never the codes (Python's `vars(login)`).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MacLogin").field("codes", &*self.codes.lock()).finish_non_exhaustive()
    }
}

impl Default for MacLogin {
    fn default() -> Self {
        Self::new()
    }
}

fn prune(codes: &mut IndexMap<String, Login>, now: f64) {
    codes.retain(|_, entry| now < entry.keep);
}

impl MacLogin {
    /// `MacLogin()`, on the monotonic clock.
    pub fn new() -> Self {
        Self::with_clock(monotonic_clock())
    }

    pub fn with_clock(clock: Clock) -> Self {
        MacLogin { clock, codes: Mutex::new(IndexMap::new()) }
    }

    /// A new code that lives LOGIN_SECS.
    pub fn issue(&self) -> String {
        self.issue_for(LOGIN_SECS)
    }

    /// A new 128-bit code, for `http://localhost:<port>/#login=<code>`.
    pub fn issue_for(&self, ttl: f64) -> String {
        let code = token_urlsafe(16);
        let now = (self.clock)();
        let mut codes = self.codes.lock();
        prune(&mut codes, now);
        while codes.len() >= MAX_LOGINS {
            codes.shift_remove_index(0); // the oldest: insertion order
        }
        let until = now + ttl.max(0.0);
        codes.insert(digest(&code), Login { until, keep: until + LOGIN_KEEP_SECS, used: false });
        code
    }

    /// True once for a code this issued, before it expires; False for anything else (None: not a string).
    pub fn consume(&self, code: Option<&str>) -> bool {
        let Some(code) = code.filter(|c| TOKEN.is_match(c)) else {
            return false;
        };
        {
            let now = (self.clock)();
            let mut codes = self.codes.lock();
            prune(&mut codes, now);
            match codes.get_mut(&digest(code)) {
                Some(entry) if !entry.used && now < entry.until => entry.used = true,
                _ => return false,
            }
        }
        tracing::info!("web auth: the Mac's browser signed in");
        true
    }

    /// Has this code been consumed? `web open` asks, to delete its file as soon as the browser signed in.
    pub fn used(&self, code: Option<&str>) -> bool {
        let Some(code) = code.filter(|c| TOKEN.is_match(c)) else {
            return false;
        };
        let now = (self.clock)();
        let mut codes = self.codes.lock();
        prune(&mut codes, now);
        codes.get(&digest(code)).is_some_and(|entry| entry.used)
    }
}

#[cfg(test)]
mod tests;
