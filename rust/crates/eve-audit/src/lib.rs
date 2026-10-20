//! Append-only JSONL log of every PreToolUse decision, plus a CLI viewer.
//!
//! Port of `eve_bridge/audit.py`.
//!
//! Useful for "what did I let through last week?" forensics — especially when
//! running with `defaultMode: bypassPermissions` + `strict = false` where the
//! matcher silently auto-approves a lot of things.
//!
//! One line per decision:
//!
//! ```text
//! {"ts": "2026-05-16T01:23:45.123+08:00", "session": "abc12345", "tool": "Bash",
//!  "hint": "git push origin main", "matcher": "ask", "decision": "allow",
//!  "source": "stick", "elapsed_s": 2.3}
//! ```
//!
//! Fields:
//! - `ts`        ISO-8601 local timestamp with offset
//! - `session`   first 8 chars of the session id (full id is noisy)
//! - `tool`      tool name (Bash / Edit / Read / ...)
//! - `hint`      short summary of what's being run (command, file path, ...)
//! - `matcher`   matcher classification: "allow" / "ask" / "default"
//! - `decision`  what the bridge actually returned: "allow" / "deny" / null
//! - `source`    how we arrived at the decision:
//!   - "auto_allow" — matcher short-circuited to allow
//!   - "telegram"   — the owner answered the yes/no on the phone
//!   - "defer"      — bridge returned no opinion (Claude Code's flow ran)
//!   - "telegram_relay" — the Claude relay is on: allowed without asking
//!   - "jev_shadow" — a second line: Jev's risk verdict for a relayed command, logged only
//!   - "jev_safe" / "jev_error" — the relay allowed after Jev judged the command safe / failed
//!   - "jev_risky_deferred" — Jev judged it risky, the phone did not answer: Claude Code's flow ran
//! - `elapsed_s` elapsed seconds for the round-trip (older entries only)
//!
//! Older logs also hold "stick" (A/B on the eve) and "timeout": the robot no longer decides prompts.
//! `jev` is `{"verdict","why","destroys","escapes","publishes","secrets","ms"}` when Jev judged the command.
//!
//! Append failures are logged once and don't propagate; the daemon must never
//! crash because of audit IO.

mod pyjson;

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, IsTerminal, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Local, Offset, TimeZone};
use serde_json::{Map, Value};

pub use pyjson::{dumps as py_json_dumps, float_repr as py_float_repr, loads_lenient, round_ndigits};

/// One parsed audit record (a JSON object, keys in file order).
pub type Entry = Map<String, Value>;

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/// Per-platform audit log location, matching the daemon log conventions.
///
/// `EVE_BRIDGE_AUDIT` (non-empty) overrides it, with `~` expanded.
pub fn default_path() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    default_path_from(
        std::env::var("EVE_BRIDGE_AUDIT").ok().as_deref(),
        std::env::var("XDG_DATA_HOME").ok().as_deref(),
        &home,
    )
}

/// [`default_path`] with the environment and home passed in (the test seam).
pub fn default_path_from(override_: Option<&str>, xdg_data_home: Option<&str>, home: &Path) -> PathBuf {
    if let Some(o) = override_.filter(|s| !s.is_empty()) {
        return normalize(Path::new(&expanduser(o)));
    }
    if cfg!(target_os = "macos") {
        return home.join("Library").join("Logs").join("eve-bridge-audit.jsonl");
    }
    if cfg!(windows) {
        return home.join("AppData").join("Local").join("eve-bridge").join("audit.jsonl");
    }
    // Linux / other Unix: XDG_DATA_HOME convention.
    let root = match xdg_data_home.filter(|s| !s.is_empty()) {
        Some(base) => normalize(Path::new(base)),
        None => home.join(".local").join("share"),
    };
    root.join("eve-bridge").join("audit.jsonl")
}

/// `os.path.expanduser`: `~` and `~/...` take `$HOME` (or the passwd entry),
/// `~name/...` takes that user's home; an unknown user leaves the path alone.
pub fn expanduser(path: &str) -> String {
    if !path.starts_with('~') {
        return path.to_string();
    }
    let i = path[1..].find('/').map(|p| p + 1).unwrap_or(path.len());
    let userhome = if i == 1 {
        match std::env::var("HOME") {
            Ok(h) => h,
            Err(_) => match dirs::home_dir() {
                Some(h) => h.to_string_lossy().into_owned(),
                None => return path.to_string(),
            },
        }
    } else {
        match user_home(&path[1..i]) {
            Some(h) => h,
            None => return path.to_string(),
        }
    };
    let joined = format!("{}{}", userhome.trim_end_matches('/'), &path[i..]);
    if joined.is_empty() {
        "/".to_string()
    } else {
        joined
    }
}

#[cfg(unix)]
fn user_home(name: &str) -> Option<String> {
    use std::ffi::{CStr, CString};
    let cname = CString::new(name).ok()?;
    let mut buf = vec![0 as libc::c_char; 16 * 1024];
    // SAFETY: passwd is plain data; getpwnam_r fills it and points into `buf`,
    // which outlives every read below.
    unsafe {
        let mut pwd: libc::passwd = std::mem::zeroed();
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        let rc = libc::getpwnam_r(cname.as_ptr(), &mut pwd, buf.as_mut_ptr(), buf.len(), &mut result);
        if rc != 0 || result.is_null() || pwd.pw_dir.is_null() {
            return None;
        }
        Some(CStr::from_ptr(pwd.pw_dir).to_string_lossy().into_owned())
    }
}

#[cfg(not(unix))]
fn user_home(_name: &str) -> Option<String> {
    None
}

/// `pathlib.Path(...)` normalization: repeated separators, `.` components
/// and a trailing separator disappear (`..` stays, as in pathlib).
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        if c != Component::CurDir {
            out.push(c.as_os_str());
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    }
}

// ---------------------------------------------------------------------------
// Recorder
// ---------------------------------------------------------------------------

/// The fields of one decision, as `AuditLog.record`'s keyword arguments.
#[derive(Debug, Clone, Copy, Default)]
pub struct Record<'a> {
    pub session_id: &'a str,
    pub tool_name: &'a str,
    pub hint: &'a str,
    pub matcher: &'a str,
    pub decision: Option<&'a str>,
    pub source: &'a str,
    pub elapsed_s: Option<f64>,
    /// The command-risk verdict beside the regex class (typed_ask.py).
    pub jev: Option<&'a Map<String, Value>>,
}

/// Append-only JSONL recorder.
///
/// Stateless beyond the cached path and a "ever failed?" sticky flag so we
/// don't spam logs with the same IOError on every PreToolUse.
#[derive(Debug)]
pub struct AuditLog {
    pub path: PathBuf,
    failure_logged: AtomicBool,
}

impl Default for AuditLog {
    fn default() -> Self {
        Self::new(None)
    }
}

impl AuditLog {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self { path: path.unwrap_or_else(default_path), failure_logged: AtomicBool::new(false) }
    }

    /// Whether a write failure has already been logged (Python's `_failure_logged`).
    pub fn failure_logged(&self) -> bool {
        self.failure_logged.load(Ordering::Relaxed)
    }

    /// Append one decision, stamped with the local time now.
    pub fn record(&self, rec: Record<'_>) {
        self.record_at(rec, &Local::now());
    }

    /// [`AuditLog::record`] with the timestamp passed in.
    pub fn record_at<Tz: TimeZone>(&self, rec: Record<'_>, now: &DateTime<Tz>)
    where
        Tz::Offset: std::fmt::Display,
    {
        let line = entry_line(rec, now);
        if let Err((at, e)) = self.append(&line) {
            if !self.failure_logged.swap(true, Ordering::Relaxed) {
                tracing::warn!(
                    "audit log write failed (will not retry-log): {}: {}",
                    self.path.display(),
                    py_oserror(&e, &at)
                );
            }
        }
    }

    fn append(&self, line: &str) -> Result<(), (PathBuf, io::Error)> {
        if let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(|e| (parent.to_path_buf(), e))?;
        }
        let mut f = OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.path)
            .map_err(|e| (self.path.clone(), e))?;
        f.write_all(line.as_bytes()).map_err(|e| (self.path.clone(), e))
    }
}

/// The exact line `AuditLog.record` appends (`json.dumps(entry, ensure_ascii=False) + "\n"`).
pub fn entry_line<Tz: TimeZone>(rec: Record<'_>, now: &DateTime<Tz>) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let mut entry = Map::new();
    entry.insert("ts".into(), Value::String(isoformat_ms(now)));
    entry.insert("session".into(), Value::String(take_chars(rec.session_id, 8)));
    entry.insert("tool".into(), Value::String(rec.tool_name.to_string()));
    entry.insert("hint".into(), Value::String(take_chars(rec.hint, 200)));
    entry.insert("matcher".into(), Value::String(rec.matcher.to_string()));
    entry.insert(
        "decision".into(),
        rec.decision.map(|d| Value::String(d.to_string())).unwrap_or(Value::Null),
    );
    entry.insert("source".into(), Value::String(rec.source.to_string()));
    let mut line = String::from("{");
    let mut first = true;
    for (k, v) in &entry {
        push_key(&mut line, &mut first, k);
        line.push_str(&pyjson::dumps(v));
    }
    if let Some(e) = rec.elapsed_s {
        // A float, so NaN/inf survive as Python writes them.
        push_key(&mut line, &mut first, "elapsed_s");
        line.push_str(&pyjson::float_repr(pyjson::round_ndigits(e, 3)));
    }
    if let Some(jev) = rec.jev.filter(|j| !j.is_empty()) {
        push_key(&mut line, &mut first, "jev");
        line.push_str(&pyjson::dumps(&Value::Object(jev.clone())));
    }
    line.push_str("}\n");
    line
}

fn push_key(line: &mut String, first: &mut bool, k: &str) {
    if !*first {
        line.push_str(", ");
    }
    *first = false;
    line.push_str(&pyjson::dumps(&Value::String(k.to_string())));
    line.push_str(": ");
}

/// `datetime.isoformat(timespec="milliseconds")` of an aware datetime.
fn isoformat_ms<Tz: TimeZone>(now: &DateTime<Tz>) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let off = now.offset().fix().local_minus_utc();
    let mut s = now.format("%Y-%m-%dT%H:%M:%S%.3f").to_string();
    let sign = if off < 0 { '-' } else { '+' };
    let a = off.unsigned_abs();
    s.push_str(&format!("{sign}{:02}:{:02}", a / 3600, (a % 3600) / 60));
    if !a.is_multiple_of(60) {
        s.push_str(&format!(":{:02}", a % 60));
    }
    s
}

/// Python's `str(OSError)`: `[Errno 21] Is a directory: '/path'`.
fn py_oserror(e: &io::Error, at: &Path) -> String {
    match e.raw_os_error() {
        Some(code) => {
            let text = io::Error::from_raw_os_error(code).to_string();
            let suffix = format!(" (os error {code})");
            let strerror = text.strip_suffix(&suffix).unwrap_or(&text);
            format!("[Errno {code}] {strerror}: '{}'", at.display())
        }
        None => e.to_string(),
    }
}

fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

// ---------------------------------------------------------------------------
// Viewer (powers `eve-bridge audit` subcommand)
// ---------------------------------------------------------------------------

// ANSI colour escapes; suppressed by `--ascii` or when stdout isn't a tty.
pub const ANSI_RESET: &str = "\x1b[0m";
pub const ANSI_DIM: &str = "\x1b[2m";
pub const ANSI_RED: &str = "\x1b[31m";
pub const ANSI_GREEN: &str = "\x1b[32m";
pub const ANSI_YELLOW: &str = "\x1b[33m";

/// The `decision` / `source` / `tool` filters (`None` = no filter).
#[derive(Debug, Clone, Copy, Default)]
pub struct Filters<'a> {
    pub decision: Option<&'a str>,
    pub source: Option<&'a str>,
    pub tool: Option<&'a str>,
}

impl Filters<'_> {
    fn matches(&self, entry: &Entry) -> bool {
        let eq = |key: &str, want: Option<&str>| match want {
            None => true,
            Some(w) => matches!(entry.get(key), Some(Value::String(s)) if s == w),
        };
        eq("decision", self.decision) && eq("source", self.source) && eq("tool", self.tool)
    }
}

/// Python `str.strip()`: Unicode whitespace plus the \x1c-\x1f separators.
fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c))
}

/// Parse one raw line (bytes up to a newline) into the matching entries it
/// holds. Universal newlines: a lone `\r` also ends a line.
fn parse_chunk(raw: &[u8], filters: &Filters<'_>, out: &mut Vec<Entry>) {
    let text = String::from_utf8_lossy(raw);
    for piece in text.split(['\r', '\n']) {
        let line = py_strip(piece);
        if line.is_empty() {
            continue;
        }
        let Some(Value::Object(entry)) = pyjson::loads_lenient(line) else {
            continue;
        };
        if filters.matches(&entry) {
            out.push(entry);
        }
    }
}

/// Streaming reader behind [`iter_entries`].
pub struct Entries {
    reader: Option<BufReader<File>>,
    open_error: Option<io::Error>,
    decision: Option<String>,
    source: Option<String>,
    tool: Option<String>,
    pending: VecDeque<Entry>,
    buf: Vec<u8>,
}

impl Iterator for Entries {
    type Item = io::Result<Entry>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(e) = self.open_error.take() {
            return Some(Err(e));
        }
        loop {
            if let Some(e) = self.pending.pop_front() {
                return Some(Ok(e));
            }
            let reader = self.reader.as_mut()?;
            self.buf.clear();
            match reader.read_until(b'\n', &mut self.buf) {
                Ok(0) => {
                    self.reader = None;
                    return None;
                }
                Ok(_) => {
                    let filters = Filters {
                        decision: self.decision.as_deref(),
                        source: self.source.as_deref(),
                        tool: self.tool.as_deref(),
                    };
                    let mut found = Vec::new();
                    parse_chunk(&self.buf, &filters, &mut found);
                    self.pending.extend(found);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    self.reader = None;
                    return Some(Err(e));
                }
            }
        }
    }
}

/// Stream parsed entries from the audit file in chronological order.
///
/// Bad lines (JSONDecodeError) are silently skipped — never crash the viewer
/// on a partially-written record at the tail. A missing file yields nothing;
/// any other IO error is yielded once, where Python's generator raises it.
pub fn iter_entries(path: Option<&Path>, filters: Filters<'_>) -> Entries {
    let target = path.map(Path::to_path_buf).unwrap_or_else(default_path);
    let (mut reader, mut open_error) = (None, None);
    if target.exists() {
        match File::open(&target) {
            Ok(f) => reader = Some(BufReader::new(f)),
            Err(e) => open_error = Some(e),
        }
    }
    Entries {
        reader,
        open_error,
        decision: filters.decision.map(str::to_string),
        source: filters.source.map(str::to_string),
        tool: filters.tool.map(str::to_string),
        pending: VecDeque::new(),
        buf: Vec::new(),
    }
}

fn fmt_decision(dec: Option<&str>, ascii_only: bool) -> String {
    let label: String = dec.filter(|d| !d.is_empty()).unwrap_or("—").chars().take(5).collect();
    let pad = format!("{label:<5}");
    if ascii_only {
        return pad;
    }
    match dec {
        Some("allow") => format!("{ANSI_GREEN}{pad}{ANSI_RESET}"),
        Some("deny") => format!("{ANSI_RED}{pad}{ANSI_RESET}"),
        _ => format!("{ANSI_DIM}{pad}{ANSI_RESET}"),
    }
}

fn fmt_source(src: &str, ascii_only: bool) -> String {
    let cut: String = src.chars().take(11).collect();
    let pad = format!("{cut:<11}");
    if ascii_only {
        return pad;
    }
    match src {
        // A person decided ("stick": older logs).
        "telegram" | "stick" => format!("{ANSI_YELLOW}{pad}{ANSI_RESET}"),
        "timeout" => format!("{ANSI_RED}{pad}{ANSI_RESET}"),
        _ => format!("{ANSI_DIM}{pad}{ANSI_RESET}"),
    }
}

fn str_field<'e>(entry: &'e Entry, key: &str) -> Option<&'e str> {
    match entry.get(key) {
        Some(Value::String(s)) => Some(s.as_str()),
        _ => None,
    }
}

/// One-line aligned rendering. Hint truncates to fill the remaining width.
pub fn format_entry(entry: &Entry, ascii_only: bool, width: usize) -> String {
    let ts = str_field(entry, "ts").unwrap_or("");
    // ts is "YYYY-MM-DDTHH:MM:SS.mmm±HH:MM" — slice out the HH:MM:SS.mmm part.
    let time_str: String = match ts.split_once('T') {
        Some((_, rest)) => rest.chars().take(12).collect(),
        None => ts.chars().take(12).collect(),
    };
    let tool: String = str_field(entry, "tool").unwrap_or("").chars().take(8).collect();
    let dec_segment = fmt_decision(str_field(entry, "decision"), ascii_only);
    let src_segment = fmt_source(str_field(entry, "source").unwrap_or(""), ascii_only);
    let mut hint = str_field(entry, "hint").unwrap_or("").replace('\n', " ");

    // Available width for the hint = total - fixed columns - separators.
    // Columns: time(12) tool(8) decision(5) source(11) = 36 + 4 spaces = 40 chars.
    let avail = (width as i64 - 40).max(20) as usize;
    if hint.chars().count() > avail {
        hint = hint.chars().take(avail - 1).collect::<String>() + "…";
    }

    format!("{time_str:<12} {tool:<8} {dec_segment} {src_segment} {hint}")
}

/// `os.get_terminal_size().columns` of stdout, or `default` when it isn't a terminal.
pub fn terminal_width(default: usize) -> usize {
    #[cfg(unix)]
    {
        // SAFETY: TIOCGWINSZ writes one winsize into the zeroed struct.
        unsafe {
            let mut ws: libc::winsize = std::mem::zeroed();
            if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) == 0 {
                return ws.ws_col as usize;
            }
        }
        default
    }
    #[cfg(not(unix))]
    {
        default
    }
}

/// Options for [`render`], as `render`'s keyword arguments.
#[derive(Debug, Clone)]
pub struct RenderOptions {
    pub path: Option<PathBuf>,
    /// Show the last N matching entries; `<= 0` shows them all.
    pub last: i64,
    pub decision: Option<String>,
    pub source: Option<String>,
    pub tool: Option<String>,
    pub ascii_only: bool,
    pub follow: bool,
    /// Follow mode stops (returning 0) once this is set — the caller's
    /// Ctrl-C handler sets it where Python caught `KeyboardInterrupt`.
    pub stop: Option<Arc<AtomicBool>>,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            path: None,
            last: 20,
            decision: None,
            source: None,
            tool: None,
            ascii_only: false,
            follow: false,
            stop: None,
        }
    }
}

/// [`render`] to the process's stdout.
pub fn render_stdout(opts: &RenderOptions) -> io::Result<i32> {
    let stdout = io::stdout();
    let tty = stdout.is_terminal();
    let mut lock = stdout.lock();
    render(opts, &mut lock, tty)
}

/// Power for the `eve-bridge audit` subcommand. `out_isatty` is `out.isatty()`.
pub fn render<W: Write>(opts: &RenderOptions, out: &mut W, out_isatty: bool) -> io::Result<i32> {
    let target = opts.path.clone().unwrap_or_else(default_path);
    let use_color = !opts.ascii_only && out_isatty;
    let width = terminal_width(120);
    let filters = Filters {
        decision: opts.decision.as_deref(),
        source: opts.source.as_deref(),
        tool: opts.tool.as_deref(),
    };

    // Header.
    if use_color {
        writeln!(out, "{ANSI_DIM}# audit log: {}{ANSI_RESET}", target.display())?;
    } else {
        writeln!(out, "# audit log: {}", target.display())?;
    }

    if !target.exists() {
        out.write_all("# (empty — no PreToolUse decisions recorded yet)\n".as_bytes())?;
        return Ok(0);
    }

    // Keep only the requested tail of matching records. The log is append-only
    // across daemon restarts, so collecting the entire history grows without
    // bound even when the viewer prints its default 20 lines.
    let matching = iter_entries(Some(&target), filters);
    if opts.last > 0 {
        let cap = opts.last as usize;
        let mut tail: VecDeque<Entry> = VecDeque::with_capacity(cap.min(1024));
        for e in matching {
            if tail.len() == cap {
                tail.pop_front();
            }
            tail.push_back(e?);
        }
        for e in &tail {
            writeln!(out, "{}", format_entry(e, !use_color, width))?;
        }
    } else {
        for e in matching {
            writeln!(out, "{}", format_entry(&e?, !use_color, width))?;
        }
    }

    if !opts.follow {
        return Ok(0);
    }

    // Follow mode: poll the file for new bytes. Cross-platform (no inotify dep).
    // Resilient to file rotation: if size shrinks, reset to start.
    out.flush()?;
    let stopped = || opts.stop.as_ref().is_some_and(|s| s.load(Ordering::Relaxed));
    let mut reader = BufReader::new(File::open(&target)?);
    reader.seek(SeekFrom::End(0))?;
    let mut buf = Vec::new();
    loop {
        if stopped() {
            return Ok(0);
        }
        buf.clear();
        let n = match reader.read_until(b'\n', &mut buf) {
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if n == 0 {
            std::thread::sleep(Duration::from_millis(500));
            // Detect truncation (rotation, manual reset).
            let Ok(meta) = fs::metadata(&target) else {
                continue;
            };
            if meta.len() < reader.stream_position()? {
                reader.seek(SeekFrom::Start(0))?;
            }
            continue;
        }
        let mut found = Vec::new();
        parse_chunk(&buf, &filters, &mut found);
        for entry in found {
            writeln!(out, "{}", format_entry(&entry, !use_color, width))?;
            out.flush()?;
        }
    }
}

#[cfg(test)]
mod tests;
