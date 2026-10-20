//! The few Python built-ins link_uploads.py leans on, with Python's exact behavior.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use serde_json::Value;
use unicode_general_category::{get_general_category, GeneralCategory as G};

/// `unicodedata.category(c)[0] == "C"`: a control, format, surrogate, private-use or unassigned character.
pub(crate) fn is_other(c: char) -> bool {
    matches!(get_general_category(c), G::Control | G::Format | G::Surrogate | G::PrivateUse | G::Unassigned)
}

/// `str.isspace()`.
pub(crate) fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `str.strip()` with no argument.
pub(crate) fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// `posixpath.splitext`: (root, ext), the extension being the last dot of the last part, leading dots ignored.
pub(crate) fn splitext(p: &str) -> (String, String) {
    let sep = p.rfind('/').map(|i| i as isize).unwrap_or(-1);
    let dot = p.rfind('.').map(|i| i as isize).unwrap_or(-1);
    if dot > sep {
        let start = (sep + 1) as usize;
        let dot = dot as usize;
        if p[start..dot].chars().any(|c| c != '.') {
            return (p[..dot].to_string(), p[dot..].to_string());
        }
    }
    (p.to_string(), String::new())
}

/// `pathlib.PurePath.suffix` (Python 3.13): the last part's final dot and after, unless the dot leads or ends it.
pub(crate) fn path_suffix(path: &Path) -> String {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    match name.rfind('.') {
        Some(i) if i > 0 && i < name.len() - 1 => name[i..].to_string(),
        _ => String::new(),
    }
}

/// Python truthiness of a JSON value.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `str(value or default)` for a value read from JSON.
pub(crate) fn str_or(v: Option<&Value>, default: &str) -> String {
    match v {
        Some(v) if truthy(v) => match v {
            Value::String(s) => s.clone(),
            Value::Bool(_) => "True".to_string(),
            other => other.to_string(),
        },
        _ => default.to_string(),
    }
}

/// `int(value or 0)` for a value read from JSON; `None` where Python raises (TypeError or ValueError).
pub(crate) fn int_or_zero(v: Option<&Value>) -> Option<i64> {
    let Some(v) = v.filter(|v| truthy(v)) else { return Some(0) };
    match v {
        Value::Bool(_) => Some(1),
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().filter(|f| f.is_finite()).map(|f| f.trunc() as i64)),
        Value::String(s) => parse_int(s, 10),
        _ => None,
    }
}

/// `int(text, base)` for base 10 or 16: surrounding whitespace, a sign, digits with single underscores between
/// them (and a `0x` prefix in base 16).
pub(crate) fn parse_int(text: &str, base: u32) -> Option<i64> {
    let s = strip(text);
    let (negative, s) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let mut s = s;
    if base == 16 && (s.starts_with("0x") || s.starts_with("0X")) {
        s = &s[2..];
        s = s.strip_prefix('_').unwrap_or(s);
    }
    if s.is_empty() || s.starts_with('_') || s.ends_with('_') || s.contains("__") {
        return None;
    }
    let mut value: i64 = 0;
    for c in s.chars() {
        if c == '_' {
            continue;
        }
        let d = c.to_digit(base)?;
        value = value.checked_mul(i64::from(base))?.checked_add(i64::from(d))?;
    }
    Some(if negative { -value } else { value })
}

/// `json.dumps(value)`: ", " and ": " separators, non-ASCII as `\uXXXX`.
pub(crate) fn dumps(v: &Value) -> String {
    let mut out = String::new();
    write_json(v, &mut out);
    out
}

fn write_json(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => write_str(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_json(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, (k, item)) in map.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_str(k, out);
                out.push_str(": ");
                write_json(item, out);
            }
            out.push('}');
        }
    }
}

fn write_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7f => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Python's `tempfile.gettempdir()`: the first of `$TMPDIR`, `$TEMP`, `$TMP`, `/tmp`, `/var/tmp`, `/usr/tmp` and
/// the working directory where a file can be created, made absolute and normalised, chosen once per process.
pub(crate) fn gettempdir() -> PathBuf {
    static TEMPDIR: OnceLock<PathBuf> = OnceLock::new();
    TEMPDIR
        .get_or_init(|| {
            let mut candidates: Vec<PathBuf> = ["TMPDIR", "TEMP", "TMP"]
                .iter()
                .filter_map(|name| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from))
                .collect();
            candidates.extend(["/tmp", "/var/tmp", "/usr/tmp"].map(PathBuf::from));
            candidates.push(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
            candidates
                .into_iter()
                .map(|dir| abspath(&dir))
                .find(|dir| writable(dir))
                // Python raises FileNotFoundError here; the store's first mkdir fails the same way.
                .unwrap_or_else(|| PathBuf::from("/tmp"))
        })
        .clone()
}

/// `os.path.abspath`: join to the working directory and normalise lexically.
fn abspath(path: &Path) -> PathBuf {
    let joined =
        if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir().unwrap_or_default().join(path) };
    let mut out = PathBuf::from("/");
    for part in joined.components() {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(p) => out.push(p),
            Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
        }
    }
    out
}

/// `tempfile._get_default_tempdir`'s probe: a new file can be created, written and removed.
fn writable(dir: &Path) -> bool {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt;

    for _ in 0..100 {
        let probe = dir.join(format!("tmp{}", &uuid::Uuid::new_v4().simple().to_string()[..8]));
        match fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&probe) {
            Ok(mut file) => {
                let ok = file.write_all(b"blat").is_ok();
                drop(file);
                let _ = fs::remove_file(&probe);
                return ok;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return false,
        }
    }
    false
}
