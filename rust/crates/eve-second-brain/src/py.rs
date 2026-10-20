//! The pieces of Python's string, path and filesystem behaviour that second_brain.py leans on, so the Rust port
//! reads and writes the same bytes: `str.strip`/`splitlines`/`repr` with Python's whitespace and line breaks,
//! code-point slicing, `Path.read_text` (UTF-8 with universal newlines), `os.path.realpath(strict=False)`,
//! Python 3.13's `Path.glob`, `fnmatch`, and OSError messages in Python's `[Errno N] text: 'path'` form.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

use parking_lot::Mutex;
use regex::Regex;

use crate::BrainError;

// ---- strings -------------------------------------------------------------------------------------

/// `str.isspace()`: Unicode White_Space plus the four information separators U+001C..U+001F.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `str.strip()`.
pub fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// `str.lstrip()`.
pub fn lstrip(s: &str) -> &str {
    s.trim_start_matches(is_space)
}

/// `str.rstrip()`.
pub fn rstrip(s: &str) -> &str {
    s.trim_end_matches(is_space)
}

/// `str.strip(chars)`.
pub fn strip_chars<'a>(s: &'a str, chars: &str) -> &'a str {
    s.trim_matches(|c| chars.contains(c))
}

/// `str.lstrip(chars)`.
pub fn lstrip_chars<'a>(s: &'a str, chars: &str) -> &'a str {
    s.trim_start_matches(|c| chars.contains(c))
}

/// `str.split()` with no separator: runs of whitespace, no empty pieces.
pub fn split_ws(s: &str) -> Vec<&str> {
    s.split(is_space).filter(|p| !p.is_empty()).collect()
}

/// `" ".join(s.split())`.
pub fn collapse_ws(s: &str) -> String {
    split_ws(s).join(" ")
}

fn is_line_break(c: char) -> bool {
    matches!(
        c,
        '\n' | '\r' | '\u{0b}' | '\u{0c}' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}'
    )
}

/// `str.splitlines()`: every Python line boundary, `\r\n` as one, no trailing empty line.
pub fn splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut iter = s.char_indices().peekable();
    while let Some((i, c)) = iter.next() {
        if is_line_break(c) {
            out.push(&s[start..i]);
            let mut end = i + c.len_utf8();
            if c == '\r' {
                if let Some(&(j, '\n')) = iter.peek() {
                    iter.next();
                    end = j + 1;
                }
            }
            start = end;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// `len(s)`: code points.
pub fn char_len(s: &str) -> usize {
    s.chars().count()
}

/// `s[:n]` for n >= 0.
pub fn clip(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// `s[n:]` for n >= 0.
pub fn skip(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[i..],
        None => "",
    }
}

/// `str.isprintable()` for one character, as far as it matters for a repr: controls, separators other than the
/// space, and the common format characters are not printable.
fn printable(c: char) -> bool {
    if c == ' ' {
        return true;
    }
    if c.is_control() || c.is_whitespace() {
        return false;
    }
    !matches!(c as u32,
        0xAD | 0x600..=0x605 | 0x61C | 0x6DD | 0x70F | 0x180E | 0x200B..=0x200F | 0x202A..=0x202E
        | 0x2060..=0x2064 | 0x2066..=0x206F | 0xFEFF | 0xFFF9..=0xFFFB | 0xE000..=0xF8FF
        | 0xF0000..=0x10FFFF)
}

/// `repr(s)` for a str.
pub fn repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if c.is_ascii() || printable(c) => out.push(c),
            c if (c as u32) <= 0xff => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if (c as u32) <= 0xffff => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push_str(&format!("\\U{:08x}", c as u32)),
        }
    }
    out.push(quote);
    out
}

/// A JSON string literal exactly as Python's `json.dumps(s, ensure_ascii=False)` writes it.
pub fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| String::from("\"\""))
}

/// `Path.stem` and `Path.suffix` (3.13): the suffix is the last `.x` of the name unless the dot leads or ends it.
pub fn stem_suffix(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 && i < name.len() - 1 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

pub fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

pub fn stem(path: &Path) -> String {
    stem_suffix(&file_name(path)).0.to_string()
}

pub fn suffix(path: &Path) -> String {
    stem_suffix(&file_name(path)).1.to_string()
}

/// `path.with_suffix(new)`.
pub fn with_suffix(path: &Path, new: &str) -> PathBuf {
    let name = file_name(path);
    path.with_file_name(format!("{}{}", stem_suffix(&name).0, new))
}

/// `PurePath(s)`: drop empty and `.` segments (a leading `.` alone stays when nothing else is left).
pub fn pure_path(s: &str) -> PathBuf {
    let p = Path::new(s);
    let mut out = PathBuf::new();
    for comp in p.components() {
        if comp != Component::CurDir {
            out.push(comp.as_os_str());
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// `Path.as_posix()` of a relative path.
pub fn posix(path: &Path) -> String {
    let parts: Vec<String> = path.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    }
}

/// `PurePath.parts` as strings (for the sort order Python gives paths: part by part).
pub fn parts(path: &Path) -> Vec<String> {
    path.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect()
}

/// `os.path.expanduser`.
pub fn expanduser(s: &str) -> String {
    if !s.starts_with('~') {
        return s.to_string();
    }
    let i = s[1..].find('/').map(|j| j + 1).unwrap_or(s.len());
    let home = if i == 1 {
        match std::env::var_os("HOME") {
            Some(h) => Some(h.to_string_lossy().into_owned()),
            None => dirs::home_dir().map(|h| h.to_string_lossy().into_owned()),
        }
    } else {
        user_home(&s[1..i])
    };
    let Some(home) = home else { return s.to_string() };
    let joined = format!("{}{}", home.trim_end_matches('/'), &s[i..]);
    if joined.is_empty() {
        "/".to_string()
    } else {
        joined
    }
}

#[cfg(unix)]
fn user_home(name: &str) -> Option<String> {
    let cname = std::ffi::CString::new(name).ok()?;
    // SAFETY: getpwnam returns a pointer into static storage or null; the home directory string is copied out
    // before anything else can call into the passwd database on this thread.
    unsafe {
        let pw = libc::getpwnam(cname.as_ptr());
        if pw.is_null() || (*pw).pw_dir.is_null() {
            return None;
        }
        Some(std::ffi::CStr::from_ptr((*pw).pw_dir).to_string_lossy().into_owned())
    }
}

#[cfg(not(unix))]
fn user_home(_name: &str) -> Option<String> {
    None
}

// ---- errors --------------------------------------------------------------------------------------

/// The OSError subclass Python raises for an errno.
fn errno_kind(code: i32) -> &'static str {
    match code {
        2 => "FileNotFoundError",
        17 => "FileExistsError",
        21 => "IsADirectoryError",
        20 => "NotADirectoryError",
        1 | 13 => "PermissionError",
        _ => "OSError",
    }
}

fn strerror(e: &io::Error) -> String {
    let text = e.to_string();
    match text.rfind(" (os error ") {
        Some(i) => text[..i].to_string(),
        None => text,
    }
}

/// An OSError for one path, worded as Python words it.
pub fn os_error(e: &io::Error, path: &Path) -> BrainError {
    match e.raw_os_error() {
        Some(code) => BrainError::Os {
            kind: errno_kind(code),
            message: format!("[Errno {code}] {}: {}", strerror(e), repr(&path.to_string_lossy())),
        },
        None => BrainError::Os { kind: "OSError", message: e.to_string() },
    }
}

/// An OSError for a two-path call (rename, replace).
pub fn os_error2(e: &io::Error, src: &Path, dst: &Path) -> BrainError {
    match e.raw_os_error() {
        Some(code) => BrainError::Os {
            kind: errno_kind(code),
            message: format!(
                "[Errno {code}] {}: {} -> {}",
                strerror(e),
                repr(&src.to_string_lossy()),
                repr(&dst.to_string_lossy())
            ),
        },
        None => BrainError::Os { kind: "OSError", message: e.to_string() },
    }
}

/// The UnicodeDecodeError (a ValueError) Python raises reading invalid UTF-8 strictly.
fn decode_error(bytes: &[u8], e: &std::str::Utf8Error) -> BrainError {
    let pos = e.valid_up_to();
    let byte = bytes[pos];
    let reason = if (0x80..=0xBF).contains(&byte) || byte >= 0xF5 || byte == 0xC0 || byte == 0xC1 {
        "invalid start byte"
    } else if e.error_len().is_none() {
        "unexpected end of data"
    } else {
        "invalid continuation byte"
    };
    BrainError::Value(format!("'utf-8' codec can't decode byte 0x{byte:02x} in position {pos}: {reason}"))
}

// ---- files ---------------------------------------------------------------------------------------

/// Universal newlines, as Python's text mode reads: `\r\n` and `\r` become `\n`.
fn universal_newlines(text: String) -> String {
    if text.contains('\r') {
        text.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        text
    }
}

/// `path.read_text(encoding="utf-8")` (strict) or with `errors="replace"`.
pub fn read_text(path: &Path, replace: bool) -> Result<String, BrainError> {
    let bytes = fs::read(path).map_err(|e| os_error(&e, path))?;
    let text = if replace {
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        match std::str::from_utf8(&bytes) {
            Ok(s) => s.to_string(),
            Err(e) => return Err(decode_error(&bytes, &e)),
        }
    };
    Ok(universal_newlines(text))
}

/// `path.write_text(text, encoding="utf-8")`.
pub fn write_text(path: &Path, text: &str) -> Result<(), BrainError> {
    fs::write(path, text).map_err(|e| os_error(&e, path))
}

/// `path.mkdir(parents=True, exist_ok=True)`.
pub fn mkdirs(path: &Path) -> Result<(), BrainError> {
    if path.is_dir() {
        return Ok(());
    }
    fs::create_dir_all(path).map_err(|e| os_error(&e, path))?;
    if path.is_dir() {
        Ok(())
    } else {
        Err(os_error(&io::Error::from_raw_os_error(17), path))
    }
}

/// `path.mkdir(mode=mode, parents=True, exist_ok=exist_ok)`: parents get the default mode, the leaf `mode`.
pub fn mkdir_mode(path: &Path, mode: u32, exist_ok: bool) -> Result<(), BrainError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            mkdirs(parent)?;
        }
    }
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    match builder.create(path) {
        Ok(()) => Ok(()),
        Err(e) if exist_ok && e.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        Err(e) => Err(os_error(&e, path)),
    }
}

/// `Path.rename(target)`.
pub fn rename(src: &Path, dst: &Path) -> Result<(), BrainError> {
    fs::rename(src, dst).map_err(|e| os_error2(&e, src, dst))
}

/// `os.replace(src, dst)`.
pub fn replace(src: &Path, dst: &Path) -> Result<(), BrainError> {
    #[cfg(test)]
    if let Some(e) = crate::seams::replace(src, dst) {
        return Err(os_error2(&e, src, dst));
    }
    fs::rename(src, dst).map_err(|e| os_error2(&e, src, dst))
}

/// `Path.unlink()`.
pub fn unlink(path: &Path) -> Result<(), BrainError> {
    fs::remove_file(path).map_err(|e| os_error(&e, path))
}

/// `path.stat().st_mtime` as a float of seconds.
pub fn mtime(path: &Path) -> Result<f64, BrainError> {
    let meta = fs::metadata(path).map_err(|e| os_error(&e, path))?;
    let modified = meta.modified().map_err(|e| os_error(&e, path))?;
    Ok(match modified.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs_f64(),
        Err(e) => -e.duration().as_secs_f64(),
    })
}

/// `path.is_symlink()`.
pub fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path).map(|m| m.file_type().is_symlink()).unwrap_or(false)
}

/// `path.iterdir()` names, unsorted; a missing or unreadable folder raises as Python's does.
pub fn iterdir(path: &Path) -> Result<Vec<PathBuf>, BrainError> {
    let entries = fs::read_dir(path).map_err(|e| os_error(&e, path))?;
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| os_error(&e, path))?;
        out.push(path.join(entry.file_name()));
    }
    Ok(out)
}

// ---- resolve ---------------------------------------------------------------------------------------

/// `Path.resolve()` (non-strict `os.path.realpath`): symlinks resolved where the path exists, `..` and `.`
/// folded, a missing tail kept as written, a symlink loop left unresolved from the loop on.
pub fn resolve(path: &Path) -> PathBuf {
    let s = path.to_string_lossy().into_owned();
    let absolute = if s.starts_with('/') {
        s
    } else {
        let cwd = std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| "/".into());
        format!("{}/{}", cwd.trim_end_matches('/'), s)
    };
    let mut seen: HashMap<String, Option<String>> = HashMap::new();
    let (out, _) = join_realpath(String::from("/"), &absolute, &mut seen);
    PathBuf::from(if out.is_empty() { "/".to_string() } else { out })
}

fn split_last(path: &str) -> (String, String) {
    match path.rfind('/') {
        Some(0) => ("/".to_string(), path[1..].to_string()),
        Some(i) => (path[..i].to_string(), path[i + 1..].to_string()),
        None => (String::new(), path.to_string()),
    }
}

fn join2(a: &str, b: &str) -> String {
    if b.starts_with('/') {
        b.to_string()
    } else if a.is_empty() || a.ends_with('/') {
        format!("{a}{b}")
    } else {
        format!("{a}/{b}")
    }
}

fn join_realpath(mut path: String, rest: &str, seen: &mut HashMap<String, Option<String>>) -> (String, bool) {
    let mut rest = rest.to_string();
    if rest.starts_with('/') {
        rest = rest[1..].to_string();
        path = "/".to_string();
    }
    while !rest.is_empty() {
        let (name, tail) = match rest.find('/') {
            Some(i) => (rest[..i].to_string(), rest[i + 1..].to_string()),
            None => (rest.clone(), String::new()),
        };
        rest = tail;
        if name.is_empty() || name == "." {
            continue;
        }
        if name == ".." {
            if !path.is_empty() {
                let (head, last) = split_last(&path);
                path = if last == ".." { join2(&join2(&head, ".."), "..") } else { head };
            } else {
                path = "..".to_string();
            }
            continue;
        }
        let newpath = join2(&path, &name);
        let is_link = fs::symlink_metadata(&newpath).map(|m| m.file_type().is_symlink()).unwrap_or(false);
        if !is_link {
            path = newpath;
            continue;
        }
        if let Some(cached) = seen.get(&newpath) {
            match cached {
                Some(p) => {
                    path = p.clone();
                    continue;
                }
                None => return (join2(&newpath, &rest), false),
            }
        }
        let target = match fs::read_link(&newpath) {
            Ok(t) => t.to_string_lossy().into_owned(),
            Err(_) => {
                path = newpath;
                continue;
            }
        };
        seen.insert(newpath.clone(), None);
        let (resolved, ok) = join_realpath(path.clone(), &target, seen);
        if !ok {
            return (join2(&resolved, &rest), false);
        }
        path = resolved.clone();
        seen.insert(newpath, Some(resolved));
    }
    (path, true)
}

/// `path.relative_to(base)` for two resolved paths, or the ValueError Python raises.
pub fn relative_to(path: &Path, base: &Path) -> Result<PathBuf, BrainError> {
    path.strip_prefix(base).map(Path::to_path_buf).map_err(|_| {
        BrainError::Value(format!(
            "{} is not in the subpath of {}",
            repr(&path.to_string_lossy()),
            repr(&base.to_string_lossy())
        ))
    })
}

// ---- fnmatch and glob ------------------------------------------------------------------------------

static FNMATCH_CACHE: LazyLock<Mutex<HashMap<String, Regex>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// One class body as Python's regex engine reads the string `fnmatch.translate` builds: escaped characters are
/// literal, `a-b` is a range, a trailing `-` is literal. None when Python's `re` would refuse it.
fn class_items(body: &[char]) -> Option<Vec<(char, char)>> {
    let mut items = Vec::new();
    let mut i = 0;
    let next = |i: &mut usize| -> Option<char> {
        if *i >= body.len() {
            return None;
        }
        let c = body[*i];
        *i += 1;
        if c == '\\' {
            let e = *body.get(*i)?;
            *i += 1;
            return Some(e);
        }
        Some(c)
    };
    while i < body.len() {
        let lo = next(&mut i)?;
        if i < body.len() && body[i] == '-' && i + 1 < body.len() {
            i += 1;
            let hi = next(&mut i)?;
            if hi < lo {
                return None;
            }
            items.push((lo, hi));
        } else {
            items.push((lo, lo));
        }
    }
    Some(items)
}

/// `fnmatch.translate(pat)` as a Rust regex: `*` any run, `?` one character, `[...]` a class with `!` negation
/// and ranges (reversed ranges dropped as Python drops them), everything else literal. Whole-string, DOTALL.
pub fn fnmatch_regex(pat: &str) -> String {
    let chars: Vec<char> = pat.chars().collect();
    let n = chars.len();
    let mut res = String::from("(?s)\\A");
    let mut i = 0;
    while i < n {
        let c = chars[i];
        i += 1;
        if c == '*' {
            while i < n && chars[i] == '*' {
                i += 1;
            }
            res.push_str(".*");
        } else if c == '?' {
            res.push('.');
        } else if c == '[' {
            let mut j = i;
            if j < n && chars[j] == '!' {
                j += 1;
            }
            if j < n && chars[j] == ']' {
                j += 1;
            }
            while j < n && chars[j] != ']' {
                j += 1;
            }
            if j >= n {
                res.push_str("\\[");
                continue;
            }
            let raw: Vec<char> = chars[i..j].to_vec();
            let mut stuff: Vec<char> = Vec::new();
            if !raw.contains(&'-') {
                for ch in raw {
                    if ch == '\\' {
                        stuff.extend(['\\', '\\']);
                    } else {
                        stuff.push(ch);
                    }
                }
            } else {
                let mut chunks: Vec<Vec<char>> = Vec::new();
                let mut ii = i;
                let mut k = if chars[i] == '!' { i + 2 } else { i + 1 };
                loop {
                    let found = if k < j { chars[k..j].iter().position(|&ch| ch == '-').map(|p| p + k) } else { None };
                    let Some(kk) = found else { break };
                    chunks.push(chars[ii..kk].to_vec());
                    ii = kk + 1;
                    k = kk + 3;
                }
                let chunk = chars[ii..j].to_vec();
                if !chunk.is_empty() {
                    chunks.push(chunk);
                } else if let Some(last) = chunks.last_mut() {
                    last.push('-');
                }
                let mut k = chunks.len();
                while k > 1 {
                    k -= 1;
                    let (prev_last, next_first) = (chunks[k - 1].last().copied(), chunks[k].first().copied());
                    if let (Some(a), Some(b)) = (prev_last, next_first) {
                        if a > b {
                            let mut merged = chunks[k - 1][..chunks[k - 1].len() - 1].to_vec();
                            merged.extend_from_slice(&chunks[k][1..]);
                            chunks[k - 1] = merged;
                            chunks.remove(k);
                        }
                    }
                }
                for (idx, chunk) in chunks.iter().enumerate() {
                    if idx > 0 {
                        stuff.push('-');
                    }
                    for &ch in chunk {
                        match ch {
                            '\\' => stuff.extend(['\\', '\\']),
                            '-' => stuff.extend(['\\', '-']),
                            _ => stuff.push(ch),
                        }
                    }
                }
            }
            let mut escaped: Vec<char> = Vec::new();
            for ch in stuff {
                if matches!(ch, '&' | '~' | '|') {
                    escaped.push('\\');
                }
                escaped.push(ch);
            }
            i = j + 1;
            if escaped.is_empty() {
                res.push_str("[^\\x{0}-\\x{10FFFF}]");
            } else if escaped == ['!'] {
                res.push('.');
            } else {
                let (negate, body): (bool, &[char]) =
                    if escaped[0] == '!' { (true, &escaped[1..]) } else { (false, &escaped[..]) };
                match class_items(body) {
                    Some(items) => {
                        res.push('[');
                        if negate {
                            res.push('^');
                        }
                        for (lo, hi) in items {
                            if lo == hi {
                                res.push_str(&format!("\\x{{{:x}}}", lo as u32));
                            } else {
                                res.push_str(&format!("\\x{{{:x}}}-\\x{{{:x}}}", lo as u32, hi as u32));
                            }
                        }
                        if res.ends_with('[') || res.ends_with("[^") {
                            // an empty class: "[^]" matches anything, "[]" nothing
                            if negate {
                                res.truncate(res.len() - 2);
                                res.push_str("[\\x{0}-\\x{10FFFF}]");
                            } else {
                                res.truncate(res.len() - 1);
                                res.push_str("[^\\x{0}-\\x{10FFFF}]");
                            }
                        } else {
                            res.push(']');
                        }
                    }
                    None => res.push_str("[^\\x{0}-\\x{10FFFF}]"),
                }
            }
        } else {
            res.push_str(&regex::escape(&c.to_string()));
        }
    }
    res.push_str("\\z");
    res
}

/// `fnmatch.fnmatch(name, pat)` on POSIX (case-sensitive, `*` crosses `/`).
pub fn fnmatch(name: &str, pat: &str) -> bool {
    let mut cache = FNMATCH_CACHE.lock();
    if let Some(re) = cache.get(pat) {
        return re.is_match(name);
    }
    let re = Regex::new(&fnmatch_regex(pat)).unwrap_or_else(|_| Regex::new(r"[^\x{0}-\x{10FFFF}]").expect("never"));
    let hit = re.is_match(name);
    if cache.len() > 256 {
        cache.clear();
    }
    cache.insert(pat.to_string(), re);
    hit
}

fn has_magic(part: &str) -> bool {
    part.contains(['*', '?', '['])
}

/// `root.glob(pattern)` as Python 3.13's pathlib does it: `**` walks every folder without following symlinked
/// folders, a wildcard segment matches names (hidden ones too) case-sensitively, literal segments are joined, and
/// a trailing `/` keeps only folders. The order is the file system's; callers sort.
pub fn glob(root: &Path, pattern: &str) -> Result<Vec<PathBuf>, BrainError> {
    if pattern.starts_with('/') {
        return Err(BrainError::Other {
            kind: "NotImplementedError",
            message: "Non-relative patterns are unsupported".to_string(),
        });
    }
    let mut parts: Vec<String> = pattern.split('/').filter(|p| !p.is_empty() && *p != ".").map(String::from).collect();
    if parts.is_empty() {
        return Err(BrainError::Value(format!("Unacceptable pattern: PosixPath({})", repr("."))));
    }
    if pattern.ends_with('/') {
        parts.push(String::new());
    }
    let mut out = Vec::new();
    select(&parts, root.to_path_buf(), false, &mut out);
    Ok(out)
}

fn select(parts: &[String], path: PathBuf, exists: bool, out: &mut Vec<PathBuf>) {
    let Some(part) = parts.first() else {
        if exists || fs::symlink_metadata(&path).is_ok() {
            out.push(path);
        }
        return;
    };
    if part == "**" {
        let mut rest = &parts[1..];
        while rest.first().map(|p| p == "**").unwrap_or(false) {
            rest = &rest[1..];
        }
        let dir_only = !rest.is_empty();
        select(rest, path.clone(), exists, out);
        let mut stack = vec![path];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                if is_dir || !dir_only {
                    let entry_path = dir.join(entry.file_name());
                    if dir_only {
                        select(rest, entry_path.clone(), true, out);
                    } else {
                        out.push(entry_path.clone());
                    }
                    if is_dir {
                        stack.push(entry_path);
                    }
                }
            }
        }
    } else if part.is_empty() || part == ".." {
        let joined = if part.is_empty() {
            PathBuf::from(format!("{}/", path.to_string_lossy().trim_end_matches('/')))
        } else {
            path.join("..")
        };
        select(&parts[1..], joined, false, out);
    } else if !has_magic(part) {
        let mut joined = path.join(part);
        let mut rest = &parts[1..];
        while let Some(next) = rest.first() {
            if has_magic(next) || next == "**" || next.is_empty() || next == ".." {
                break;
            }
            joined = joined.join(next);
            rest = &rest[1..];
        }
        select(rest, joined, false, out);
    } else {
        let dir_only = parts.len() > 1;
        let Ok(entries) = fs::read_dir(&path) else { return };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !fnmatch(&name, part) {
                continue;
            }
            let entry_path = path.join(entry.file_name());
            if dir_only {
                if !entry_path.is_dir() {
                    continue;
                }
                select(&parts[1..], entry_path, true, out);
            } else {
                out.push(entry_path);
            }
        }
    }
}
