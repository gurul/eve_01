//! Load `~/.config/eve-bridge/env` into the process environment.
//!
//! Port of `eve_bridge/envfile.py`.
//!
//! The launchd service starts the daemon with a fixed, minimal environment, so
//! secrets such as `OPENAI_API_KEY` cannot come from the shell. They live in
//! a `KEY=VALUE` file the user owns (mode 600 recommended) and are read here
//! once at startup. Values already present in the environment win — the file
//! fills gaps, it never overrides.
//!
//! File format: one `KEY=VALUE` per line. Blank lines and lines starting
//! with `#` are ignored, a leading `export ` is tolerated, and a value
//! wrapped in matching single or double quotes is unquoted. Nothing else is
//! interpreted — no inline comments, no `$VAR` expansion.
//!
//! Values are never logged. Only the key names are.
//!
//! Writing ([`set_key`], [`remove_key`]) is for eve lite's setup page, which saves the key OpenRouter mints
//! instead of asking anyone to paste one. A write replaces the file atomically, leaves it owner-only (0600) and
//! keeps every line it does not set, comments included.
//!
//! Line splitting, whitespace trimming, `~` expansion and symlink resolution follow Python's `str.splitlines`,
//! `str.strip`, `pathlib.Path.expanduser` and `os.path.realpath` exactly, so both daemons read and write the
//! same file the same way.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use parking_lot::Mutex;
use rand::Rng;

/// The env file the daemon reads when `EVE_ENV_FILE` is unset or empty (before `~` expansion).
pub const DEFAULT_PATH: &str = "~/.config/eve-bridge/env";

/// The env var that names another env file.
pub const ENV_FILE_VAR: &str = "EVE_ENV_FILE";

/// What Python's `str.splitlines()` ends a line on, so what [`parse_env_file`] would read as the start of
/// another line.
pub const LINE_BREAKS: [char; 10] = [
    '\n', '\r', '\u{0b}', '\u{0c}', '\u{1c}', '\u{1d}', '\u{1e}', '\u{85}', '\u{2028}', '\u{2029}',
];

static WRITE_LOCK: Mutex<()> = Mutex::new(());

/// The errors the env file functions raise. Python raises `ValueError` for the first three (a
/// `UnicodeDecodeError` is a `ValueError`), `RuntimeError` for [`EnvFileError::HomeDir`] and `OSError` for
/// [`EnvFileError::Io`].
#[derive(Debug, thiserror::Error)]
pub enum EnvFileError {
    #[error("env file: a name is letters, digits and _, and does not start with a digit")]
    InvalidName,
    #[error("env file: the value for {name} must be text on one line, with no NUL")]
    InvalidValue { name: String },
    /// The file is not UTF-8. The message is the one Python's `UnicodeDecodeError` carries.
    #[error("{0}")]
    Decode(String),
    /// `~` or `~user` could not be expanded (pathlib's `RuntimeError`).
    #[error("Could not determine home directory.")]
    HomeDir,
    /// A value with a NUL cannot go into the process environment (`os.environ`'s `ValueError`).
    #[error("embedded null byte")]
    EmbeddedNull,
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl EnvFileError {
    /// True for what Python raises as `ValueError` (a bad name, a bad value, undecodable bytes).
    pub fn is_value_error(&self) -> bool {
        matches!(self, Self::InvalidName | Self::InvalidValue { .. } | Self::Decode(_) | Self::EmbeddedNull)
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The environment a load reads and fills: Python's `MutableMapping[str, str]`, `os.environ` by default.

/// A string-to-string mapping [`load_env_file`] fills and [`env_path`] reads, as Python's `environ` argument.
pub trait Environ {
    /// The value of `key`, if set.
    fn var(&self, key: &str) -> Option<OsString>;
    /// Whether `key` is set (`key in environ`).
    fn contains_var(&self, key: &str) -> bool {
        self.var(key).is_some()
    }
    /// `environ[key] = value`. Only the process environment can refuse one (a NUL in it).
    fn set_var(&mut self, key: &str, value: &str) -> Result<(), EnvFileError>;
}

/// The process environment, Python's `os.environ`. Setting a variable mutates the process environment, so
/// load it at startup before other threads read the environment, as the daemon does.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessEnv;

impl Environ for ProcessEnv {
    fn var(&self, key: &str) -> Option<OsString> {
        std::env::var_os(key)
    }
    fn set_var(&mut self, key: &str, value: &str) -> Result<(), EnvFileError> {
        // `std::env::set_var` panics where `os.environ` raises: on a NUL, or a key that is empty or holds "=".
        if key.is_empty() || key.contains('=') || key.contains('\0') || value.contains('\0') {
            return Err(EnvFileError::EmbeddedNull);
        }
        std::env::set_var(key, value);
        Ok(())
    }
}

impl<S: std::hash::BuildHasher> Environ for HashMap<String, String, S> {
    fn var(&self, key: &str) -> Option<OsString> {
        self.get(key).map(OsString::from)
    }
    fn set_var(&mut self, key: &str, value: &str) -> Result<(), EnvFileError> {
        self.insert(key.to_string(), value.to_string());
        Ok(())
    }
}

impl Environ for std::collections::BTreeMap<String, String> {
    fn var(&self, key: &str) -> Option<OsString> {
        self.get(key).map(OsString::from)
    }
    fn set_var(&mut self, key: &str, value: &str) -> Result<(), EnvFileError> {
        self.insert(key.to_string(), value.to_string());
        Ok(())
    }
}

impl<S: std::hash::BuildHasher> Environ for IndexMap<String, String, S> {
    fn var(&self, key: &str) -> Option<OsString> {
        self.get(key).map(OsString::from)
    }
    fn set_var(&mut self, key: &str, value: &str) -> Result<(), EnvFileError> {
        self.insert(key.to_string(), value.to_string());
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Python string semantics the format depends on.

/// Python's `str.isspace()` for one character: Rust's `char::is_whitespace` plus U+001C..U+001F, which Python
/// also counts as whitespace.
pub fn is_py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Python's `str.strip()`.
pub fn py_strip(s: &str) -> &str {
    s.trim_matches(is_py_space)
}

/// Python's `str.lstrip()`.
pub fn py_lstrip(s: &str) -> &str {
    s.trim_start_matches(is_py_space)
}

/// Python's `str.splitlines()`: splits on every [`LINE_BREAKS`] character, `\r\n` counting as one break; no
/// trailing empty line after a final break.
pub fn py_splitlines(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut iter = text.char_indices().peekable();
    while let Some((i, c)) = iter.next() {
        if LINE_BREAKS.contains(&c) {
            out.push(&text[start..i]);
            let mut end = i + c.len_utf8();
            if c == '\r' {
                if let Some(&(_, '\n')) = iter.peek() {
                    iter.next();
                    end += 1;
                }
            }
            start = end;
        }
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

/// `^[A-Za-z_][A-Za-z0-9_]*$`, matched against the whole string.
fn is_key(s: &str) -> bool {
    let mut cs = s.chars();
    match cs.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => cs.all(|c| c.is_ascii_alphanumeric() || c == '_'),
        _ => false,
    }
}

/// At least two characters, the first and last the same quote, `'` or `"`.
fn quote_wrapped(value: &str) -> bool {
    let mut cs = value.chars();
    match (cs.next(), cs.next_back()) {
        (Some(first), Some(last)) => first == last && (first == '\'' || first == '"'),
        _ => false,
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Reading.

/// `KEY=VALUE` lines -> map, in the order names first appear (a later line for a name replaces its value, as a
/// Python dict does). Malformed lines are skipped, not fatal.
pub fn parse_env_file(text: &str) -> IndexMap<String, String> {
    let mut out = IndexMap::new();
    for raw in py_splitlines(text) {
        let mut line = py_strip(raw);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("export ") {
            line = py_lstrip(rest);
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = py_strip(key);
        if !is_key(key) {
            continue;
        }
        let mut value = py_strip(value);
        if quote_wrapped(value) {
            // Both ends are one-byte ASCII quotes.
            value = &value[1..value.len() - 1];
        }
        out.insert(key.to_string(), value.to_string());
    }
    out
}

/// Read the env file into `environ` without overriding existing keys.
///
/// `path` is the file; `None` means `EVE_ENV_FILE` from `environ` when set and not empty, else
/// [`DEFAULT_PATH`]. `~` is expanded from the process's `HOME`.
///
/// Returns the names that were set. A missing or unreadable file sets nothing and returns an empty list — the
/// file is optional. A file that is not UTF-8 is an error ([`EnvFileError::Decode`]), as Python's
/// `UnicodeDecodeError` escapes the loader, and so is a value with a NUL loaded into the process environment
/// ([`EnvFileError::EmbeddedNull`]).
pub fn load_env_file<E: Environ + ?Sized>(path: Option<&Path>, environ: &mut E) -> Result<Vec<String>, EnvFileError> {
    load_env_file_in(path, environ, process_home())
}

fn load_env_file_in<E: Environ + ?Sized>(
    path: Option<&Path>,
    environ: &mut E,
    home: Option<OsString>,
) -> Result<Vec<String>, EnvFileError> {
    let p = match path {
        Some(p) => expanduser_in(p, home)?,
        None => configured_path(environ, home)?,
    };
    let text = match fs::read(&p) {
        Ok(bytes) => decode_utf8_sig(bytes)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            tracing::warn!("env file: cannot read {}: {}", p.display(), py_oserror(&e, &p));
            return Ok(Vec::new());
        }
    };
    let mut loaded = Vec::new();
    for (key, value) in parse_env_file(&text) {
        if environ.contains_var(&key) {
            continue;
        }
        // A refused value stops the load there, as os.environ's ValueError escapes Python's loop; the names
        // already set stay set.
        environ.set_var(&key, &value)?;
        loaded.push(key);
    }
    if !loaded.is_empty() {
        tracing::info!("env file: loaded {} from {}", loaded.join(", "), p.display());
    }
    Ok(loaded)
}

/// The env file this process reads: `EVE_ENV_FILE` when set (and not empty), else the default, as
/// [`load_env_file`] picks it. Fails only when `~` cannot be expanded.
pub fn env_path<E: Environ + ?Sized>(environ: &E) -> Result<PathBuf, EnvFileError> {
    configured_path(environ, process_home())
}

fn configured_path<E: Environ + ?Sized>(environ: &E, home: Option<OsString>) -> Result<PathBuf, EnvFileError> {
    let raw = environ
        .var(ENV_FILE_VAR)
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| OsString::from(DEFAULT_PATH));
    expanduser_in(Path::new(&raw), home)
}

// ---------------------------------------------------------------------------------------------------------------
// Writing.

/// Set `name` to `value` in the env file at `path`.
///
/// The first line that sets `name` (`export` form included) is replaced where it is, later ones are dropped,
/// and a new name goes at the end. Every other line stays as it was: comments, blanks, other keys and lines the
/// parser skips. The value is written so [`parse_env_file`] reads it back unchanged: it is quoted only where
/// the parser would otherwise trim or unquote it. A missing file is created, in a new folder made 0700 if need
/// be; a symlinked file is written through to its target.
///
/// Fails with a value error for a name the parser would not read or a value holding a line break or NUL (it
/// would start a second line), and the file is left as it was; an unreadable file is never overwritten. Only
/// the name is logged.
pub fn set_key(path: &Path, name: &str, value: &str) -> Result<(), EnvFileError> {
    set_key_with(path, name, value, &os_replace, process_home())
}

fn set_key_with(
    path: &Path,
    name: &str,
    value: &str,
    rename: &dyn Fn(&Path, &Path) -> io::Result<()>,
    home: Option<OsString>,
) -> Result<(), EnvFileError> {
    check_name(name)?;
    if value.contains('\0') || value.chars().any(|c| LINE_BREAKS.contains(&c)) {
        return Err(EnvFileError::InvalidValue { name: name.to_string() });
    }
    let target = real(path, home)?;
    let line = format!("{name}={}", quoted(value));
    {
        let _guard = WRITE_LOCK.lock();
        let mut lines: Vec<String> = Vec::new();
        let mut done = false;
        for raw in read_lines(&target)? {
            if name_of(&raw).as_deref() == Some(name) {
                if !done {
                    lines.push(line.clone());
                    done = true;
                }
                continue;
            }
            lines.push(raw);
        }
        if !done {
            lines.push(line);
        }
        replace_file(&target, &lines, rename)?;
    }
    tracing::info!("env file: set {} in {}", name, target.display());
    Ok(())
}

/// Delete every line that sets `name` from the env file at `path`. True when there was one. When there was
/// none, or no file, nothing is written and the answer is false. A rewritten file is 0600, as [`set_key`]
/// leaves it.
pub fn remove_key(path: &Path, name: &str) -> Result<bool, EnvFileError> {
    remove_key_in(path, name, process_home())
}

fn remove_key_in(path: &Path, name: &str, home: Option<OsString>) -> Result<bool, EnvFileError> {
    check_name(name)?;
    let target = real(path, home)?;
    {
        let _guard = WRITE_LOCK.lock();
        let lines = read_lines(&target)?;
        let total = lines.len();
        let kept: Vec<String> = lines.into_iter().filter(|raw| name_of(raw).as_deref() != Some(name)).collect();
        if kept.len() == total {
            return Ok(false);
        }
        replace_file(&target, &kept, &os_replace)?;
    }
    tracing::info!("env file: removed {} from {}", name, target.display());
    Ok(true)
}

fn check_name(name: &str) -> Result<(), EnvFileError> {
    // A full match, not Python's re.match: "$" also matches before a final newline, and "NAME\n" would split
    // the line it writes.
    if is_key(name) {
        Ok(())
    } else {
        Err(EnvFileError::InvalidName)
    }
}

/// The name a line sets, read by the parser [`load_env_file`] uses; `None` for a comment, a blank or a line
/// the parser skips.
fn name_of(raw: &str) -> Option<String> {
    parse_env_file(raw).into_keys().next()
}

/// `value` as the parser reads it back unchanged. The parser trims the value and strips one pair of matching
/// quotes, so a value it would change is wrapped in double quotes; anything inside them is kept as it is.
fn quoted(value: &str) -> String {
    if value != py_strip(value) || quote_wrapped(value) {
        format!("\"{value}\"")
    } else {
        value.to_string()
    }
}

fn real(path: &Path, home: Option<OsString>) -> Result<PathBuf, EnvFileError> {
    let expanded = expanduser_in(path, home)?;
    Ok(realpath(&expanded)?)
}

/// The file's lines, or none when there is no file. Any other failure to read it, undecodable bytes included,
/// is an error: rewriting a file eve could not read would lose what it holds.
fn read_lines(path: &Path) -> Result<Vec<String>, EnvFileError> {
    match fs::read(path) {
        Ok(bytes) => {
            let text = decode_utf8_sig(bytes)?;
            Ok(py_splitlines(&text).into_iter().map(str::to_string).collect())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}

fn os_replace(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)
}

/// `lines` into `path` through a 0600 temporary file in the same folder and one rename, so a reader sees the
/// old file or the new one, never part of one.
fn replace_file(path: &Path, lines: &[String], rename: &dyn Fn(&Path, &Path) -> io::Result<()>) -> io::Result<()> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    if !parent.is_dir() {
        mkdir_parents(parent, 0o700)?;
    }
    let file_name = path.file_name().unwrap_or_else(|| OsStr::new(""));
    let (mut file, tmp) = mkstemp(parent, file_name)?;
    let result = (|| -> io::Result<()> {
        let mut body = String::new();
        for line in lines {
            body.push_str(line);
            body.push('\n');
        }
        file.write_all(body.as_bytes())?;
        file.flush()?;
        fsync_file(&file)?;
        drop(file);
        set_mode(&tmp, 0o600)?;
        rename(&tmp, path)
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    sync_folder(parent);
    Ok(())
}

/// pathlib's `mkdir(mode, parents=True, exist_ok=True)`: the folder itself gets `mode`, the parents it creates
/// on the way get the default mode.
fn mkdir_parents(dir: &Path, mode: u32) -> io::Result<()> {
    match mkdir_mode(dir, mode) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            match dir.parent() {
                Some(parent) if parent != dir && !parent.as_os_str().is_empty() => {
                    mkdir_parents(parent, 0o777)?;
                }
                _ => return Err(e),
            }
            match mkdir_mode(dir, mode) {
                Err(_) if dir.is_dir() => Ok(()),
                other => other,
            }
        }
        Err(_) if dir.is_dir() => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(unix)]
fn mkdir_mode(dir: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(mode).create(dir)
}

#[cfg(not(unix))]
fn mkdir_mode(dir: &Path, _mode: u32) -> io::Result<()> {
    fs::create_dir(dir)
}

/// `tempfile.mkstemp(prefix=f".{name}.", suffix=".tmp", dir=folder)`: a new 0600 file, opened exclusively,
/// named from the same 37 characters Python draws from.
fn mkstemp(dir: &Path, name: &OsStr) -> io::Result<(fs::File, PathBuf)> {
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789_";
    const TMP_MAX: usize = 10000;
    let mut rng = rand::rng();
    for _ in 0..TMP_MAX {
        let random: String = (0..8).map(|_| CHARS[rng.random_range(0..CHARS.len())] as char).collect();
        let mut file_name = OsString::from(".");
        file_name.push(name);
        file_name.push(".");
        file_name.push(&random);
        file_name.push(".tmp");
        let tmp = dir.join(file_name);
        match open_exclusive(&tmp) {
            Ok(f) => return Ok((f, tmp)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "No usable temporary file name found"))
}

#[cfg(unix)]
fn open_exclusive(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn open_exclusive(path: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new().read(true).write(true).create_new(true).open(path)
}

/// `os.fsync`: plain fsync(2), as Python does (Rust's `sync_all` uses `F_FULLFSYNC` on macOS).
#[cfg(unix)]
fn fsync_file(file: &fs::File) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    // SAFETY: the descriptor is owned by `file` and open for the duration of the call.
    if unsafe { libc::fsync(file.as_raw_fd()) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn fsync_file(file: &fs::File) -> io::Result<()> {
    file.sync_all()
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

/// Flush the folder too, so the rename survives a power cut. Skipped where a folder cannot be opened.
#[cfg(unix)]
fn sync_folder(folder: &Path) {
    use std::os::unix::io::AsRawFd;
    let Ok(dir) = fs::File::open(folder) else {
        return;
    };
    // SAFETY: the descriptor is owned by `dir` and open for the duration of the call. A failure is ignored.
    unsafe {
        libc::fsync(dir.as_raw_fd());
    }
}

#[cfg(not(unix))]
fn sync_folder(_folder: &Path) {}

// ---------------------------------------------------------------------------------------------------------------
// Bytes, text and Python's error text.

/// `read_text(encoding="utf-8-sig")`: one leading BOM dropped, the rest strict UTF-8. A file that is only the
/// start of a BOM (`EF` or `EF BB`) reads as empty text: Python's incremental `utf-8-sig` decoder holds those
/// bytes back waiting for more, and the final read drops them without an error.
fn decode_utf8_sig(mut bytes: Vec<u8>) -> Result<String, EnvFileError> {
    const BOM: &[u8] = b"\xEF\xBB\xBF";
    if !bytes.is_empty() && bytes.len() < BOM.len() && BOM.starts_with(&bytes) {
        return Ok(String::new());
    }
    if bytes.starts_with(BOM) {
        bytes.drain(..3);
    }
    String::from_utf8(bytes).map_err(|e| {
        let bytes = e.as_bytes();
        let err = e.utf8_error();
        EnvFileError::Decode(utf8_error_message(bytes, err.valid_up_to(), err.error_len()))
    })
}

/// The text of the `UnicodeDecodeError` Python's UTF-8 codec raises for the first bad sequence.
fn utf8_error_message(bytes: &[u8], at: usize, error_len: Option<usize>) -> String {
    let start = bytes[at];
    match error_len {
        None if bytes.len() - at > 1 => format!(
            "'utf-8' codec can't decode bytes in position {}-{}: unexpected end of data",
            at,
            bytes.len() - 1
        ),
        None => format!("'utf-8' codec can't decode byte 0x{start:02x} in position {at}: unexpected end of data"),
        // A valid lead and continuation bytes cut short by a bad one: Python names the whole run.
        Some(n) if n > 1 => format!(
            "'utf-8' codec can't decode bytes in position {}-{}: invalid continuation byte",
            at,
            at + n - 1
        ),
        Some(_) => {
            let reason = if matches!(start, 0x80..=0xC1 | 0xF5..=0xFF) {
                "invalid start byte"
            } else {
                "invalid continuation byte"
            };
            format!("'utf-8' codec can't decode byte 0x{start:02x} in position {at}: {reason}")
        }
    }
}

/// `str(OSError)` as Python prints it: `[Errno 13] Permission denied: '/path'`.
fn py_oserror(e: &io::Error, path: &Path) -> String {
    match e.raw_os_error() {
        Some(code) => {
            let text = io::Error::from_raw_os_error(code).to_string();
            let strerror = text.split(" (os error").next().unwrap_or(&text);
            format!("[Errno {code}] {strerror}: {}", py_repr(&path.to_string_lossy()))
        }
        None => e.to_string(),
    }
}

/// Python's `repr()` of a str, for the common characters.
fn py_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
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
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

// ---------------------------------------------------------------------------------------------------------------
// Paths: pathlib's parsing and `expanduser`, and `os.path.realpath`.

#[cfg(unix)]
fn os_bytes(s: &OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    s.as_bytes().to_vec()
}

#[cfg(not(unix))]
fn os_bytes(s: &OsStr) -> Vec<u8> {
    s.to_string_lossy().into_owned().into_bytes()
}

#[cfg(unix)]
fn path_from_bytes(b: Vec<u8>) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(OsString::from_vec(b))
}

#[cfg(not(unix))]
fn path_from_bytes(b: Vec<u8>) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(&b).into_owned())
}

fn process_home() -> Option<OsString> {
    std::env::var_os("HOME")
}

/// pathlib's POSIX parse: the root (`""`, `"/"`, or `"//"` for exactly two leading slashes) and the parts,
/// with empty and `.` parts dropped.
fn parse_parts(raw: &[u8]) -> (&'static [u8], Vec<Vec<u8>>) {
    let root: &'static [u8] = if raw.starts_with(b"//") && !raw.starts_with(b"///") {
        b"//"
    } else if raw.starts_with(b"/") {
        b"/"
    } else {
        b""
    };
    let parts = raw
        .split(|b| *b == b'/')
        .filter(|p| !p.is_empty() && *p != b".")
        .map(<[u8]>::to_vec)
        .collect();
    (root, parts)
}

/// `str(Path(...))` from a root and parts.
fn join_parts(root: &[u8], parts: &[Vec<u8>]) -> PathBuf {
    if root.is_empty() && parts.is_empty() {
        return PathBuf::from(".");
    }
    let mut out = root.to_vec();
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            out.push(b'/');
        }
        out.extend_from_slice(part);
    }
    path_from_bytes(out)
}

/// `Path(path).expanduser()` with `~` read from the process's `HOME` (or the password database when `HOME` is
/// unset), and `~user` from the password database. The result is normalized as pathlib normalizes it. A home
/// that cannot be found is [`EnvFileError::HomeDir`].
pub fn expanduser(path: &Path) -> Result<PathBuf, EnvFileError> {
    expanduser_in(path, process_home())
}

fn expanduser_in(path: &Path, home: Option<OsString>) -> Result<PathBuf, EnvFileError> {
    let raw = os_bytes(path.as_os_str());
    let (root, parts) = parse_parts(&raw);
    if !root.is_empty() || parts.is_empty() || !parts[0].starts_with(b"~") {
        return Ok(join_parts(root, &parts));
    }
    let first = &parts[0];
    let user = &first[1..];
    let userhome: Option<Vec<u8>> = if user.is_empty() {
        match home {
            Some(h) => Some(os_bytes(&h)),
            None => passwd_home(None),
        }
    } else {
        passwd_home(Some(user))
    };
    // os.path.expanduser returns the part unchanged when it cannot find a home; pathlib then refuses it.
    let homedir = match userhome {
        None => return Err(EnvFileError::HomeDir),
        Some(mut h) => {
            while h.last() == Some(&b'/') {
                h.pop();
            }
            if h.is_empty() {
                b"/".to_vec()
            } else {
                h
            }
        }
    };
    if homedir.starts_with(b"~") {
        return Err(EnvFileError::HomeDir);
    }
    let (home_root, mut home_parts) = parse_parts(&homedir);
    home_parts.extend(parts[1..].iter().cloned());
    Ok(join_parts(home_root, &home_parts))
}

/// `pwd.getpwuid(os.getuid()).pw_dir` (`user` None) or `pwd.getpwnam(user).pw_dir`; `None` when there is no
/// such entry.
#[cfg(unix)]
fn passwd_home(user: Option<&[u8]>) -> Option<Vec<u8>> {
    use std::ffi::{CStr, CString};
    let name = match user {
        Some(u) => Some(CString::new(u).ok()?),
        None => None,
    };
    let mut buf: Vec<libc::c_char> = vec![0; 4096];
    loop {
        // SAFETY: `pwd` and `result` are plain out-parameters; `buf` outlives the call and its length is passed.
        let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        let rc = unsafe {
            match &name {
                None => libc::getpwuid_r(libc::getuid(), &mut pwd, buf.as_mut_ptr(), buf.len(), &mut result),
                Some(n) => libc::getpwnam_r(n.as_ptr(), &mut pwd, buf.as_mut_ptr(), buf.len(), &mut result),
            }
        };
        if rc == libc::ERANGE && buf.len() < (1 << 20) {
            let len = buf.len() * 2;
            buf.resize(len, 0);
            continue;
        }
        if rc != 0 || result.is_null() || pwd.pw_dir.is_null() {
            return None;
        }
        // SAFETY: on success pw_dir points into `buf`, NUL-terminated.
        return Some(unsafe { CStr::from_ptr(pwd.pw_dir) }.to_bytes().to_vec());
    }
}

#[cfg(not(unix))]
fn passwd_home(user: Option<&[u8]>) -> Option<Vec<u8>> {
    if user.is_some() {
        return None;
    }
    std::env::var_os("USERPROFILE").map(|h| os_bytes(&h))
}

enum Part {
    Name(Vec<u8>),
    /// A symlink whose target has now been resolved: record where it led.
    Resolved(Vec<u8>),
}

/// `os.path.realpath(path)` (non-strict, Python 3.13): the absolute path with every symlink resolved; parts
/// that do not exist are kept as written, `..` is applied to the path resolved so far, and a symlink loop is
/// left unresolved rather than an error. Fails only when the working directory cannot be read.
pub fn realpath(path: &Path) -> io::Result<PathBuf> {
    let filename = os_bytes(path.as_os_str());
    let mut rest: Vec<Part> = filename.split(|b| *b == b'/').rev().map(|p| Part::Name(p.to_vec())).collect();
    let mut part_count = rest.len();
    let mut resolved: Vec<u8> = if filename.starts_with(b"/") {
        b"/".to_vec()
    } else {
        os_bytes(std::env::current_dir()?.as_os_str())
    };
    let mut seen: HashMap<Vec<u8>, Option<Vec<u8>>> = HashMap::new();
    while part_count > 0 {
        let Some(part) = rest.pop() else { break };
        let name = match part {
            Part::Resolved(link) => {
                seen.insert(link, Some(resolved.clone()));
                continue;
            }
            Part::Name(name) => name,
        };
        part_count -= 1;
        if name.is_empty() || name == b"." {
            continue;
        }
        if name == b".." {
            let cut = resolved.iter().rposition(|b| *b == b'/').unwrap_or(0);
            resolved.truncate(cut);
            if resolved.is_empty() {
                resolved.push(b'/');
            }
            continue;
        }
        let mut newpath = resolved.clone();
        if newpath != b"/" {
            newpath.push(b'/');
        }
        newpath.extend_from_slice(&name);
        let new = path_from_bytes(newpath.clone());
        match fs::symlink_metadata(&new) {
            Ok(m) if m.file_type().is_symlink() => {}
            _ => {
                resolved = newpath;
                continue;
            }
        }
        if let Some(cached) = seen.get(&newpath) {
            // A known link: use where it led, or, still unresolved, it is a loop and stays as written.
            resolved = cached.clone().unwrap_or(newpath);
            continue;
        }
        let target = match fs::read_link(&new) {
            Ok(t) => os_bytes(t.as_os_str()),
            Err(_) => {
                resolved = newpath;
                continue;
            }
        };
        if target.starts_with(b"/") {
            resolved = b"/".to_vec();
        }
        seen.insert(newpath.clone(), None);
        rest.push(Part::Resolved(newpath));
        let before = rest.len();
        rest.extend(target.split(|b| *b == b'/').rev().map(|p| Part::Name(p.to_vec())));
        part_count += rest.len() - before;
    }
    Ok(path_from_bytes(resolved))
}

#[cfg(test)]
mod tests;
