//! Locating Claude Code's config home(s). Port of `eve_bridge/claude_home.py`.
//!
//! Claude Code reads its settings and writes its transcripts under `$CLAUDE_CONFIG_DIR` when that variable is
//! set, and under `~/.claude` otherwise. Wrappers set it: era-code runs sessions against a private config home,
//! and anything else driving Claude Code with its own profile does the same.
//!
//! Hardcoding `~/.claude` fails silently and confusingly:
//!
//! * hooks get installed into a file the running sessions never read, so the board never prompts and nothing
//!   logs an error;
//! * the transcript tailer watches an empty tree, so token counts and the entry feed stay at zero while the
//!   session is plainly busy.
//!
//! The daemon is the awkward case. It is one long-lived background process (a launchd agent / systemd unit)
//! that outlives and out-scopes any single session, so it cannot see a per-session `$CLAUDE_CONFIG_DIR`. It
//! therefore takes a list: `$EVE_CLAUDE_CONFIG_DIRS` (`os.pathsep`-separated) enumerates every home to serve,
//! which is how one daemon covers both a plain `~/.claude` and a wrapper's private home at the same time.
//!
//! Every function has a `_in` form that reads its variables from a [`HomeEnv`] rather than the process
//! environment, so tests (and callers that carry their own environment) never touch the process's variables.
//! Paths are built the way Python 3.13's `pathlib.PosixPath` builds them: `~` and `~user` expanded as
//! `Path.expanduser` does, then normalized (`a//b/./c/` is `a/b/c`, exactly two leading slashes are kept), so
//! a path printed by either daemon is the same string.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// The variable naming every config home the daemon serves, `os.pathsep`-separated.
pub const MULTI_ENV: &str = "EVE_CLAUDE_CONFIG_DIRS";

/// The variable Claude Code itself reads for its single config home.
pub const SINGLE_ENV: &str = "CLAUDE_CONFIG_DIR";

/// `os.pathsep`.
#[cfg(not(windows))]
const PATHSEP: u8 = b':';
#[cfg(windows)]
const PATHSEP: u8 = b';';

/// pathlib's `RuntimeError("Could not determine home directory.")`, raised when `~` (or `~user`) names a home
/// that neither `$HOME` nor the password database can supply.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Could not determine home directory.")]
pub struct HomeDirError;

/// The environment these lookups read: `$CLAUDE_CONFIG_DIR`, `$EVE_CLAUDE_CONFIG_DIRS` and `$HOME`.
///
/// `None` is an unset variable. `$HOME` unset means the password database is asked, as Python does.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HomeEnv {
    pub claude_config_dir: Option<OsString>,
    pub claude_config_dirs: Option<OsString>,
    pub home: Option<OsString>,
}

impl HomeEnv {
    /// The variables as the process currently has them.
    pub fn from_process() -> Self {
        Self {
            claude_config_dir: std::env::var_os(SINGLE_ENV),
            claude_config_dirs: std::env::var_os(MULTI_ENV),
            home: std::env::var_os("HOME"),
        }
    }

    /// `Path.home()`: `~` expanded.
    pub fn home_dir(&self) -> Result<PathBuf, HomeDirError> {
        expanduser_in(Path::new("~"), self.home.as_deref())
    }

    /// `Path(path).expanduser()`, with `~` read from this environment's `$HOME`.
    pub fn expanduser(&self, path: &Path) -> Result<PathBuf, HomeDirError> {
        expanduser_in(path, self.home.as_deref())
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The public lookups, each in a process-environment form and an explicit-environment `_in` form.

/// The single config home to act on.
///
/// Precedence: explicit override > `$CLAUDE_CONFIG_DIR` > `~/.claude`. Used by session-scoped commands (hook
/// install/uninstall/status), which run inside the same environment as the sessions they configure.
pub fn claude_config_dir(override_: Option<&OsStr>) -> Result<PathBuf, HomeDirError> {
    claude_config_dir_in(&HomeEnv::from_process(), override_)
}

/// [`claude_config_dir`] against an explicit environment.
pub fn claude_config_dir_in(env: &HomeEnv, override_: Option<&OsStr>) -> Result<PathBuf, HomeDirError> {
    let base = py_strip(&first_truthy(override_, env.claude_config_dir.as_deref()));
    if base.is_empty() {
        Ok(join(&env.home_dir()?, ".claude"))
    } else {
        env.expanduser(&path_from_bytes(base))
    }
}

/// Every config home to watch, de-duplicated, order preserved.
///
/// Precedence: explicit override (`os.pathsep`-separated) > `$EVE_CLAUDE_CONFIG_DIRS` > the single home from
/// [`claude_config_dir`]. Used by the daemon, which serves whatever sessions happen to connect.
pub fn claude_config_dirs(override_: Option<&OsStr>) -> Result<Vec<PathBuf>, HomeDirError> {
    claude_config_dirs_in(&HomeEnv::from_process(), override_)
}

/// [`claude_config_dirs`] against an explicit environment.
pub fn claude_config_dirs_in(env: &HomeEnv, override_: Option<&OsStr>) -> Result<Vec<PathBuf>, HomeDirError> {
    let raw = py_strip(&first_truthy(override_, env.claude_config_dirs.as_deref()));
    if raw.is_empty() {
        return Ok(vec![claude_config_dir_in(env, None)?]);
    }
    let mut out: Vec<PathBuf> = Vec::new();
    for part in raw.split(|b| *b == PATHSEP) {
        let part = py_strip(part);
        if part.is_empty() {
            continue;
        }
        let p = env.expanduser(&path_from_bytes(part))?;
        push_unique(&mut out, p);
    }
    if out.is_empty() {
        return Ok(vec![claude_config_dir_in(env, None)?]);
    }
    Ok(out)
}

/// Homes to bake into a service definition at install time.
///
/// Distinct from [`claude_config_dirs`], which is what the *running* daemon reads. `install --service` is
/// typically run from inside a wrapper's session, so `$CLAUDE_CONFIG_DIR` names that wrapper's private home —
/// taking it alone would silently drop plain `~/.claude` sessions from the daemon's view. Serve the union
/// instead: whatever the installing session uses, plus the default home.
pub fn daemon_config_dirs(override_: Option<&OsStr>) -> Result<Vec<PathBuf>, HomeDirError> {
    daemon_config_dirs_in(&HomeEnv::from_process(), override_)
}

/// [`daemon_config_dirs`] against an explicit environment.
pub fn daemon_config_dirs_in(env: &HomeEnv, override_: Option<&OsStr>) -> Result<Vec<PathBuf>, HomeDirError> {
    let raw = py_strip(&first_truthy(override_, env.claude_config_dirs.as_deref()));
    if !raw.is_empty() {
        let raw = os_from_bytes(raw);
        return claude_config_dirs_in(env, Some(&raw));
    }
    let mut out = vec![claude_config_dir_in(env, None)?];
    let default = join(&env.home_dir()?, ".claude");
    push_unique(&mut out, default);
    Ok(out)
}

/// Where to install hooks: `settings.json`.
///
/// Measured, not assumed. `settings.local.json` looked like the safer home — wrappers own settings.json and
/// era-code rewrites it wholesale — but an A/B test settled it: with hooks ONLY in settings.local.json no hook
/// fired at all, and adding them to settings.json made prompts work immediately. Claude Code does not read the
/// local file from a $CLAUDE_CONFIG_DIR home.
///
/// So the collision with era-code has to be fixed on era-code's side (it now preserves third-party hooks when
/// regenerating this file) rather than by hiding in a file nothing reads.
pub fn settings_path(override_: Option<&OsStr>) -> Result<PathBuf, HomeDirError> {
    settings_path_in(&HomeEnv::from_process(), override_)
}

/// [`settings_path`] against an explicit environment.
pub fn settings_path_in(env: &HomeEnv, override_: Option<&OsStr>) -> Result<PathBuf, HomeDirError> {
    Ok(join(&claude_config_dir_in(env, override_)?, "settings.json"))
}

/// Each config home's `projects` folder, where Claude Code writes its transcripts.
pub fn transcript_roots(override_: Option<&OsStr>) -> Result<Vec<PathBuf>, HomeDirError> {
    transcript_roots_in(&HomeEnv::from_process(), override_)
}

/// [`transcript_roots`] against an explicit environment.
pub fn transcript_roots_in(env: &HomeEnv, override_: Option<&OsStr>) -> Result<Vec<PathBuf>, HomeDirError> {
    Ok(claude_config_dirs_in(env, override_)?.into_iter().map(|d| join(&d, "projects")).collect())
}

// ---------------------------------------------------------------------------------------------------------------
// Python semantics.

/// `override or os.environ.get(NAME) or ""`: the first value that is set and non-empty.
fn first_truthy(override_: Option<&OsStr>, env: Option<&OsStr>) -> Vec<u8> {
    [override_, env]
        .into_iter()
        .flatten()
        .find(|v| !v.is_empty())
        .map(os_bytes)
        .unwrap_or_default()
}

/// `list.append` guarded by `not in`: pathlib equality on POSIX is equality of the normalized string.
fn push_unique(out: &mut Vec<PathBuf>, p: PathBuf) {
    if !out.iter().any(|q| q.as_os_str() == p.as_os_str()) {
        out.push(p);
    }
}

/// `path / name` for a single plain name.
fn join(base: &Path, name: &str) -> PathBuf {
    let mut b = os_bytes(base.as_os_str());
    if b == b"." {
        // Path(".") / "x" is "x".
        return PathBuf::from(name);
    }
    if !b.ends_with(b"/") {
        b.push(b'/');
    }
    b.extend_from_slice(name.as_bytes());
    path_from_bytes(b)
}

/// `str.isspace` for one character.
fn py_isspace(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n'
            | '\u{0b}'
            | '\u{0c}'
            | '\r'
            | '\u{1c}'..='\u{1f}'
            | ' '
            | '\u{85}'
            | '\u{a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
    )
}

/// The character the bytes start with, if they start with a whole UTF-8 character, and its length.
fn leading_char(b: &[u8]) -> Option<(char, usize)> {
    (1..=b.len().min(4)).find_map(|n| std::str::from_utf8(&b[..n]).ok().and_then(|s| s.chars().next().map(|c| (c, n))))
}

/// The character the bytes end with, if they end with a whole UTF-8 character, and its length.
fn trailing_char(b: &[u8]) -> Option<(char, usize)> {
    (1..=b.len().min(4))
        .find_map(|n| std::str::from_utf8(&b[b.len() - n..]).ok().and_then(|s| s.chars().next().map(|c| (c, n))))
}

/// `str.strip()` on an environment value as Python decodes it (UTF-8 with surrogateescape: an undecodable byte
/// is never whitespace).
fn py_strip(b: &[u8]) -> Vec<u8> {
    let mut s = b;
    while let Some((c, n)) = leading_char(s) {
        if !py_isspace(c) {
            break;
        }
        s = &s[n..];
    }
    while let Some((c, n)) = trailing_char(s) {
        if !py_isspace(c) {
            break;
        }
        s = &s[..s.len() - n];
    }
    s.to_vec()
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
    let parts = raw.split(|b| *b == b'/').filter(|p| !p.is_empty() && *p != b".").map(<[u8]>::to_vec).collect();
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

/// `Path(path).expanduser()` with `~` read from `home` (the password database when it is `None`) and `~user`
/// from the password database, normalized as pathlib normalizes it.
fn expanduser_in(path: &Path, home: Option<&OsStr>) -> Result<PathBuf, HomeDirError> {
    let raw = os_bytes(path.as_os_str());
    let (root, parts) = parse_parts(&raw);
    if !root.is_empty() || parts.is_empty() || !parts[0].starts_with(b"~") {
        return Ok(join_parts(root, &parts));
    }
    let user = &parts[0][1..];
    let userhome: Option<Vec<u8>> = if user.is_empty() {
        match home {
            Some(h) => Some(os_bytes(h)),
            None => passwd_home(None),
        }
    } else {
        passwd_home(Some(user))
    };
    // os.path.expanduser returns the part unchanged when it cannot find a home; pathlib then refuses it.
    let mut homedir = userhome.ok_or(HomeDirError)?;
    while homedir.last() == Some(&b'/') {
        homedir.pop();
    }
    if homedir.is_empty() {
        homedir = b"/".to_vec();
    }
    if homedir.starts_with(b"~") {
        return Err(HomeDirError);
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
fn os_from_bytes(b: Vec<u8>) -> OsString {
    use std::os::unix::ffi::OsStringExt;
    OsString::from_vec(b)
}

#[cfg(not(unix))]
fn os_from_bytes(b: Vec<u8>) -> OsString {
    OsString::from(String::from_utf8_lossy(&b).into_owned())
}

fn path_from_bytes(b: Vec<u8>) -> PathBuf {
    PathBuf::from(os_from_bytes(b))
}

#[cfg(test)]
mod tests;
