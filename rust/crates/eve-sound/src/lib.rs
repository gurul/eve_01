//! Sound: the owner can mute eve. Muted means silent, not still — the head
//! and the LEDs keep moving; only sound stops.
//!
//! Every sound eve makes comes from the board's chirp synth, and the firmware
//! already gates all of it on its persisted `sound` setting (NVS key
//! `s_snd`). The host sets that setting with `{"cmd":"sound","on":bool}` and
//! re-sends it on every connect, so a reflashed or replaced board comes back
//! with the owner's choice. The choice itself is kept here, in
//! `~/.config/eve-bridge/sound.json`, so it survives a daemon restart.
//!
//! Two host-side belts on top of the firmware switch: caption pages go out with
//! `"chirp": false` while muted (older firmware without the command stays
//! quiet during a conversation), and the voice never plays audio on the Mac
//! speaker in `EVE_VOICE_OUTPUT=audio` mode.
//!
//! Port of `bridge/src/eve_bridge/sound.py`.

use std::borrow::Cow;
use std::collections::HashMap;
use std::ffi::{CStr, CString, OsString};
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

/// The process home, as `Path.home()` finds it: `$HOME`, else the passwd entry.
fn home() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(h) => PathBuf::from(h),
        None => dirs::home_dir().unwrap_or_default(),
    }
}

/// The home directory of `user` from the passwd database (`pwd.getpwnam`).
fn passwd_home(user: &[u8]) -> Option<PathBuf> {
    let name = CString::new(user).ok()?;
    let mut buf: Vec<libc::c_char> = vec![0; 4096];
    loop {
        // SAFETY: an all-zero passwd is a valid out-parameter for getpwnam_r.
        let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: every pointer is valid for the duration of the call and buf.len() is its true size.
        let rc = unsafe { libc::getpwnam_r(name.as_ptr(), &mut pwd, buf.as_mut_ptr(), buf.len(), &mut result) };
        if rc == libc::ERANGE && buf.len() < 1 << 20 {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        if rc != 0 || result.is_null() || pwd.pw_dir.is_null() {
            return None;
        }
        // SAFETY: pw_dir points into buf, NUL-terminated, while buf lives.
        let dir = unsafe { CStr::from_ptr(pwd.pw_dir) };
        return Some(PathBuf::from(OsString::from_vec(dir.to_bytes().to_vec())));
    }
}

/// `Path.expanduser()`: a leading `~` or `~user` becomes that home directory.
///
/// pathlib raises when `~user` names no account; this keeps the path as
/// written instead (see PARITY.md, Differences).
fn expanduser(p: &Path) -> PathBuf {
    let bytes = p.as_os_str().as_bytes();
    if bytes.first() != Some(&b'~') {
        return p.to_path_buf();
    }
    let end = bytes.iter().position(|&b| b == b'/').unwrap_or(bytes.len());
    let user = &bytes[1..end];
    let homedir = if user.is_empty() { Some(home()) } else { passwd_home(user) };
    let Some(homedir) = homedir else {
        return p.to_path_buf();
    };
    let rest = bytes[end..].iter().position(|&b| b != b'/').map(|i| &bytes[end + i..]);
    match rest {
        Some(rest) => homedir.join(std::ffi::OsStr::from_bytes(rest)),
        None => homedir,
    }
}

/// `~/.config/eve-bridge/<name>.json`, or `EVE_<NAME>_FILE`.
///
/// `environ` stands in for `os.environ` when given.
pub fn default_path(environ: Option<&HashMap<String, String>>, name: &str) -> PathBuf {
    let key = format!("EVE_{}_FILE", name.to_uppercase());
    let raw = match environ {
        Some(env) => env.get(&key).cloned(),
        None => std::env::var(&key).ok(),
    }
    .unwrap_or_default();
    let raw = raw.trim();
    if !raw.is_empty() {
        return expanduser(Path::new(raw));
    }
    home().join(".config").join("eve-bridge").join(format!("{name}.json"))
}

/// `{"cmd": "sound", "on": <on>}`, the firmware's mute command.
pub fn build_sound_cmd(on: bool) -> Value {
    json!({"cmd": "sound", "on": on})
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

/// A caption page without its talk chirp while muted; unchanged otherwise.
///
/// The original is never modified: a muted page with a chirp comes back as an
/// owned copy (keys in the same order), anything else as the same borrow.
pub fn quiet_caption(msg: &Map<String, Value>, muted: bool) -> Cow<'_, Map<String, Value>> {
    if muted && msg.get("chirp").is_some_and(truthy) {
        let mut out = msg.clone();
        out.insert("chirp".to_string(), Value::Bool(false));
        return Cow::Owned(out);
    }
    Cow::Borrowed(msg)
}

/// Why the stored setting could not be read.
enum ReadError {
    /// No file: sound is on, nothing to say.
    Missing,
    /// Any other failure, with the text the warning carries.
    Bad(String),
}

/// The persisted `on` value at `path`.
fn read_on(path: &Path) -> Result<bool, ReadError> {
    let bytes = std::fs::read(path).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => ReadError::Missing,
        _ => ReadError::Bad(e.to_string()),
    })?;
    let text = String::from_utf8(bytes).map_err(|e| ReadError::Bad(e.to_string()))?;
    let obj: Value = serde_json::from_str(&text).map_err(|e| ReadError::Bad(e.to_string()))?;
    let Value::Object(obj) = obj else {
        return Err(ReadError::Bad("expected a JSON object".to_string()));
    };
    Ok(!obj.get("muted").is_some_and(truthy))
}

/// On or off, persisted. A missing or unreadable file means on.
///
/// `name` is the switch's name in log lines and its default file: the
/// daemon keeps two, `sound` (mute) and `mic` (the owner's microphone
/// switch, see ears.py).
#[derive(Debug, Clone)]
pub struct SoundSetting {
    pub name: String,
    pub path: PathBuf,
    pub on: bool,
}

impl SoundSetting {
    /// `SoundSetting(path, name)`: `path` defaults to [`default_path`] for `name`.
    pub fn new(path: Option<PathBuf>, name: &str) -> Self {
        let path = path.unwrap_or_else(|| default_path(None, name));
        Self { name: name.to_string(), path, on: true }
    }

    /// The `sound` switch at `path`.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self::new(Some(path.into()), "sound")
    }

    pub fn muted(&self) -> bool {
        !self.on
    }

    pub fn load(&mut self) -> bool {
        self.on = match read_on(&self.path) {
            Ok(on) => on,
            Err(ReadError::Missing) => true,
            Err(ReadError::Bad(e)) => {
                tracing::warn!("{}: cannot read {} ({}) — {} stays on", self.name, self.path.display(), e, self.name);
                true
            }
        };
        self.on
    }

    /// Change and persist. Returns true when the value changed.
    pub fn set(&mut self, on: bool) -> bool {
        let changed = on != self.on;
        self.on = on;
        if let Err(e) = self.save() {
            tracing::warn!(
                "{}: cannot save {} ({}) — the choice lasts until the daemon restarts",
                self.name,
                self.path.display(),
                e
            );
        }
        changed
    }

    fn save(&self) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        // Path.with_suffix(".tmp"): sound.json -> sound.tmp
        let tmp = self.path.with_extension("tmp");
        // json.dumps({"muted": ...}) + "\n"
        let body = format!("{{\"muted\": {}}}\n", !self.on);
        std::fs::write(&tmp, body)?;
        std::fs::rename(&tmp, &self.path)
    }
}

#[cfg(test)]
mod tests;
