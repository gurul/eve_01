//! Optional, private API-key identity for Eve's Codex children.
//!
//! The default environment is deliberately identical to the plan-backed adapter.
//! Only the explicit setup command copies runtime configuration or performs login.
//!
//! Port of `eve_bridge/codex_auth.py`.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use parking_lot::Mutex;
use regex::Regex;

pub const ENV_KEYS: [&str; 7] = ["HOME", "PATH", "USER", "LOGNAME", "TMPDIR", "LANG", "CODEX_HOME"];
// Inspected in the desktop distribution: config selects marketplaces/plugins;
// plugins/cache contains manifests and .mcp.json, and .plugin-appserver its runtime.
// The generated bundled marketplace is also referenced by config.toml. No session
// databases, desktop IPC, OAuth credentials or account caches belong in this copy.
pub const COPY_PATHS: [&str; 6] = [
    "config.toml",
    "computer-use",
    "browser",
    "plugins",
    ".tmp/bundled-marketplaces",
    "chrome-native-hosts-v2.json",
];
pub const EXCLUDED_NAMES: [&str; 3] = ["auth.json", ".credentials.json", ".remote-plugin-install-staging"];

/// The reasons already logged in this process (Python's module-level `_warned`).
pub type Warned = Mutex<HashSet<String>>;
static WARNED: LazyLock<Warned> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// An actionable setup failure, with no subprocess output or credentials.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct SetupError(pub String);

impl SetupError {
    fn new(message: &str) -> Self {
        SetupError(message.to_string())
    }
}

/// Everything `setup` can raise. The CLI catches all three, as the Python
/// `except (SetupError, OSError, ValueError)` does, and prints only a
/// `SetupError` message verbatim.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Python's `SetupError`.
    #[error(transparent)]
    Setup(#[from] SetupError),
    /// Python's `OSError`.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// Python's `ValueError` (TOML decode errors, non-UTF-8 text).
    #[error("{0}")]
    Value(String),
}

/// Which identity a Codex child runs under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    Plan,
    Api,
}

impl AuthMode {
    pub fn as_str(self) -> &'static str {
        match self {
            AuthMode::Plan => "plan",
            AuthMode::Api => "api",
        }
    }
}

impl std::fmt::Display for AuthMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl PartialEq<&str> for AuthMode {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

/// `Path.home()`: `$HOME`, else the password database.
fn home_dir() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(h) if !h.is_empty() => PathBuf::from(h),
        _ => dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")),
    }
}

/// `~/.config/eve-bridge/codex-home`.
pub fn eve_home() -> PathBuf {
    eve_home_in(&home_dir())
}

/// `eve_home` for an explicit home directory.
pub fn eve_home_in(home: &Path) -> PathBuf {
    home.join(".config").join("eve-bridge").join("codex-home")
}

fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path).map(|m| m.file_type().is_symlink()).unwrap_or(false)
}

/// Python's `str.isspace` for one character.
fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

fn py_strip(s: &str) -> &str {
    s.trim_matches(py_isspace)
}

/// Require a local API identity, never accept copied ChatGPT credentials.
pub fn api_ready(home: &Path) -> bool {
    let auth = home.join("auth.json");
    if is_symlink(home) || is_symlink(&auth) {
        return false;
    }
    match fs::metadata(&auth) {
        Ok(m) if m.nlink() == 1 => {}
        _ => return false,
    }
    let Ok(bytes) = fs::read(&auth) else { return false };
    let Ok(text) = String::from_utf8(bytes) else { return false };
    let Ok(data) = serde_json::from_str::<serde_json::Value>(&text) else { return false };
    let Some(obj) = data.as_object() else { return false };
    obj.get("auth_mode").and_then(|v| v.as_str()) == Some("apikey")
        && obj
            .get("OPENAI_API_KEY")
            .and_then(|v| v.as_str())
            .is_some_and(|k| !py_strip(k).is_empty())
}

/// Log `reason` once per `warned` set. Returns whether it logged.
fn once(warned: &Warned, reason: &str) -> bool {
    let mut seen = warned.lock();
    if seen.contains(reason) {
        return false;
    }
    seen.insert(reason.to_string());
    tracing::warn!(
        "codex: {}; using the ChatGPT plan. Run eve-bridge codex-home for API setup.",
        reason
    );
    true
}

fn process_env() -> HashMap<String, String> {
    std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
        .collect()
}

/// The environment and auth mode for a Codex child. `environ` defaults to the
/// process environment.
pub fn launch_environment(environ: Option<&HashMap<String, String>>) -> (IndexMap<String, String>, AuthMode) {
    match environ {
        Some(env) => launch_environment_with(env, &eve_home(), &WARNED),
        None => launch_environment_with(&process_env(), &eve_home(), &WARNED),
    }
}

/// `launch_environment` with the Eve home and the warn-once set made explicit.
pub fn launch_environment_with(
    env: &HashMap<String, String>,
    eve_home: &Path,
    warned: &Warned,
) -> (IndexMap<String, String>, AuthMode) {
    let mut child = child_env(env);
    let mode = env.get("EVE_CODEX_AUTH").map(String::as_str).unwrap_or("plan");
    if mode == "api" {
        if api_ready(eve_home) {
            child.insert("CODEX_HOME".into(), eve_home.to_string_lossy().into_owned());
            return (child, AuthMode::Api);
        }
        once(warned, "the Eve API home is missing or not signed in with an API key");
    } else if mode != "plan" {
        once(warned, "EVE_CODEX_AUTH must be plan or api");
    }
    (child, AuthMode::Plan)
}

fn child_env(env: &HashMap<String, String>) -> IndexMap<String, String> {
    ENV_KEYS
        .iter()
        .filter_map(|k| env.get(*k).map(|v| (k.to_string(), v.clone())))
        .collect()
}

/// `Path.absolute()`.
fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map(|c| c.join(path)).unwrap_or_else(|_| path.to_path_buf())
    }
}

/// `Path.resolve()` (non-strict): `posixpath.realpath` without requiring the
/// path to exist; a symlink loop leaves the rest unresolved, as Python does.
pub fn resolve(path: &Path) -> PathBuf {
    let mut seen: HashMap<PathBuf, Option<PathBuf>> = HashMap::new();
    let (p, _) = join_realpath(PathBuf::new(), &absolute(path), &mut seen);
    if p.as_os_str().is_empty() {
        PathBuf::from("/")
    } else {
        p
    }
}

fn join_rest(mut path: PathBuf, rest: &[OsString]) -> PathBuf {
    for r in rest {
        path.push(r);
    }
    path
}

fn join_realpath(
    mut path: PathBuf,
    rest: &Path,
    seen: &mut HashMap<PathBuf, Option<PathBuf>>,
) -> (PathBuf, bool) {
    if rest.is_absolute() {
        path = PathBuf::from("/");
    }
    let names: Vec<OsString> = rest
        .components()
        .filter_map(|c| match c {
            Component::Normal(n) => Some(n.to_os_string()),
            Component::ParentDir => Some(OsString::from("..")),
            _ => None,
        })
        .collect();
    for (i, name) in names.iter().enumerate() {
        if name == ".." {
            if path.as_os_str().is_empty() {
                path = PathBuf::from("..");
            } else if path.ends_with("..") {
                path.push("..");
            } else {
                path.pop();
            }
            continue;
        }
        let newpath = path.join(name);
        let is_link = fs::symlink_metadata(&newpath).map(|m| m.file_type().is_symlink()).unwrap_or(false);
        if !is_link {
            path = newpath;
            continue;
        }
        if let Some(known) = seen.get(&newpath) {
            match known {
                Some(resolved) => {
                    path = resolved.clone();
                    continue;
                }
                None => return (join_rest(newpath, &names[i + 1..]), false),
            }
        }
        seen.insert(newpath.clone(), None);
        let target = match fs::read_link(&newpath) {
            Ok(t) => t,
            Err(_) => {
                path = newpath;
                continue;
            }
        };
        let (resolved, ok) = join_realpath(path, &target, seen);
        if !ok {
            return (join_rest(resolved, &names[i + 1..]), false);
        }
        seen.insert(newpath, Some(resolved.clone()));
        path = resolved;
    }
    (path, true)
}

fn any_symlink_in(path: &Path) -> bool {
    path.ancestors().any(is_symlink)
}

fn private_directory(path: &Path) -> Result<(), Error> {
    // Reject symlink ancestors as well as the final path before any write/login.
    if any_symlink_in(path) {
        return Err(SetupError::new("Eve Codex home must not be a symlink or have symlink parents.").into());
    }
    if let Err(e) = fs::DirBuilder::new().mode(0o700).create(path) {
        match e.kind() {
            io::ErrorKind::NotFound => {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                if let Err(e) = fs::DirBuilder::new().mode(0o700).create(path) {
                    if !path.is_dir() {
                        return Err(e.into());
                    }
                }
            }
            _ if path.is_dir() => {}
            _ => return Err(e.into()),
        }
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// `Path.read_text()`: strict UTF-8 with universal newlines.
fn read_text(path: &Path) -> Result<String, Error> {
    let bytes = fs::read(path)?;
    let text = String::from_utf8(bytes)
        .map_err(|_| Error::Value(format!("{} is not valid UTF-8", path.display())))?;
    Ok(if text.contains('\r') { text.replace("\r\n", "\n").replace('\r', "\n") } else { text })
}

fn same_file(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino()
}

/// `shutil.copy2`: contents, permission bits and timestamps, following links.
fn copy2(source: &Path, target: &Path) -> io::Result<()> {
    fs::copy(source, target)?;
    let meta = fs::metadata(source)?;
    let times = fs::FileTimes::new().set_accessed(meta.accessed()?).set_modified(meta.modified()?);
    // A read-only fd is enough to set times on a file we own (a 0444 copy stays settable).
    fs::File::open(target)?.set_times(times)?;
    Ok(())
}

fn excluded(name: &std::ffi::OsStr) -> bool {
    EXCLUDED_NAMES.iter().any(|n| name == *n)
}

/// Dereference only in-home runtime links; never leave a write-through link.
fn copy_runtime(
    source: &Path,
    target: &Path,
    source_home: &Path,
    dest_home: &Path,
    ancestors: &HashSet<PathBuf>,
) -> Result<(), Error> {
    if source.file_name().is_some_and(excluded) {
        return Ok(());
    }
    let real = resolve(source);
    if !real.starts_with(source_home) || real.file_name().is_some_and(excluded) {
        return Err(SetupError::new("Codex runtime contains a link outside its home or to credentials.").into());
    }
    if ancestors.contains(&real) {
        return Err(SetupError::new("Codex runtime contains a cyclic directory link.").into());
    }
    if source.is_dir() {
        fs::create_dir_all(target)?;
        let mut inner = ancestors.clone();
        inner.insert(real);
        for item in fs::read_dir(source)? {
            let item = item?;
            copy_runtime(&item.path(), &target.join(item.file_name()), source_home, dest_home, &inner)?;
        }
    } else if source.is_file() {
        let personal_auth = source_home.join("auth.json");
        if let Ok(auth_meta) = fs::metadata(&personal_auth) {
            if same_file(&fs::metadata(source)?, &auth_meta) {
                return Err(SetupError::new("Codex runtime contains a hard link to personal credentials.").into());
            }
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        copy2(source, target)?;
        // Desktop-generated MCP/marketplace files also embed CODEX_HOME. Repoint
        // only our copies. Application binaries and their signatures stay intact.
        let suffix = source.extension().and_then(|e| e.to_str());
        if matches!(suffix, Some("json" | "toml")) {
            let text = read_text(target)?;
            let updated = text.replace(&*source_home.to_string_lossy(), &dest_home.to_string_lossy());
            if updated != text {
                fs::write(target, updated)?;
            }
        }
    }
    Ok(())
}

/// `str.splitlines(keepends=True)`.
fn splitlines_keepends(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let end = match c {
            '\r' => match chars.peek() {
                Some((j, '\n')) => {
                    let j = *j;
                    chars.next();
                    j + 1
                }
                _ => i + 1,
            },
            '\n' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}' => {
                i + c.len_utf8()
            }
            _ => continue,
        };
        out.push(&s[start..end]);
        start = end;
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

fn parse_toml(text: &str) -> Result<toml::Table, Error> {
    // The decoder's message quotes the offending line, which may hold a secret.
    text.parse::<toml::Table>()
        .map_err(|_| Error::Value("Codex config.toml is not valid TOML.".into()))
}

const API_CONFIG_KEYS: [&str; 3] = ["cli_auth_credentials_store", "forced_login_method", "forced_chatgpt_workspace_id"];
static API_CONFIG_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"^[\s\x1c-\x1f]*(?:{})[\s\x1c-\x1f]*=", API_CONFIG_KEYS.join("|"))).expect("valid regex")
});

fn api_config(path: &Path) -> Result<(), Error> {
    let original = if path.exists() { read_text(path)? } else { String::new() };
    let config = parse_toml(&original)?;
    // Login and runtime must both use the private auth.json, including on machines
    // where the owner's ordinary Codex uses the OS keychain.
    let mut output = String::new();
    let mut in_table = false;
    for line in splitlines_keepends(&original) {
        if line.trim_start_matches(py_isspace).starts_with('[') {
            in_table = true;
        }
        if !in_table && API_CONFIG_LINE.is_match(line) {
            continue;
        }
        output.push_str(line);
    }
    let new = format!("cli_auth_credentials_store = \"file\"\nforced_login_method = \"api\"\n{output}");
    let mut expected: toml::Table =
        config.into_iter().filter(|(k, _)| !API_CONFIG_KEYS.contains(&k.as_str())).collect();
    expected.insert("cli_auth_credentials_store".into(), toml::Value::String("file".into()));
    expected.insert("forced_login_method".into(), toml::Value::String("api".into()));
    match parse_toml(&new) {
        Ok(parsed) if parsed == expected => {}
        _ => return Err(SetupError::new("Could not safely adapt Codex credential storage configuration.").into()),
    }
    fs::write(path, new)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

/// Refresh runtime copies and log in once, exclusively inside Eve's home.
/// `environ` defaults to the process environment.
pub fn setup(binary: &str, environ: Option<&HashMap<String, String>>) -> Result<PathBuf, Error> {
    match environ {
        Some(env) => setup_in(&home_dir(), binary, env),
        None => setup_in(&home_dir(), binary, &process_env()),
    }
}

/// `setup` with the user's home directory made explicit.
pub fn setup_in(home: &Path, binary: &str, env: &HashMap<String, String>) -> Result<PathBuf, Error> {
    let source = home.join(".codex");
    let destination = eve_home_in(home);
    if is_symlink(&source) || resolve(&destination) == resolve(&source) {
        return Err(SetupError::new("Eve Codex home must be separate from the personal Codex home.").into());
    }
    private_directory(&destination)?;
    let auth = destination.join("auth.json");
    if is_symlink(&auth) {
        return Err(SetupError::new("Eve auth.json must not be a symlink.").into());
    }
    if auth.exists() && fs::metadata(&auth)?.nlink() != 1 {
        return Err(SetupError::new("Eve auth.json must not be a hard link.").into());
    }
    let signed_in = api_ready(&destination);
    let key = py_strip(env.get("OPENAI_API_KEY").map(String::as_str).unwrap_or("")).to_string();
    if !signed_in && key.is_empty() {
        return Err(SetupError::new("Set OPENAI_API_KEY in the Eve env file before running codex-home.").into());
    }
    // Prepare copies before replacing existing runtime directories. auth.json is
    // never a copy target. Refreshing an existing home preserves its API sign-in.
    {
        let parent = destination.parent().unwrap_or(Path::new("/"));
        let tmp = tempfile::Builder::new().prefix(".codex-setup-").tempdir_in(parent)?;
        let staging = tmp.path();
        let source_home = resolve(&source);
        let dest_home = resolve(&destination);
        for relative in COPY_PATHS {
            let path = source.join(relative);
            if path.exists() {
                copy_runtime(&path, &staging.join(relative), &source_home, &dest_home, &HashSet::new())?;
            }
        }
        api_config(&staging.join("config.toml"))?;
        for relative in COPY_PATHS {
            let (copied, target) = (staging.join(relative), destination.join(relative));
            if !copied.exists() {
                continue;
            }
            if any_symlink_in(&target) {
                return Err(SetupError::new("Eve runtime destination must not contain symlinks.").into());
            }
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            if target.is_dir() {
                fs::remove_dir_all(&target)?;
            } else if target.exists() {
                fs::remove_file(&target)?;
            }
            fs::rename(&copied, &target)?;
        }
        tmp.close()?;
    }
    if signed_in {
        return Ok(destination);
    }
    let mut child = child_env(env);
    child.insert("CODEX_HOME".into(), destination.to_string_lossy().into_owned());
    // No shell, no key in argv, and no raw login output in logs (even on error).
    let could_not_run = || SetupError::new("Codex API login could not run; check the installed Codex executable.");
    let code = run_login(binary, &child, &key, Duration::from_secs(60)).map_err(|_| could_not_run())?;
    if code != Some(0) || !api_ready(&destination) {
        return Err(SetupError::new("Codex API login failed; Eve will keep using the plan until setup succeeds.").into());
    }
    fs::set_permissions(destination.join("auth.json"), fs::Permissions::from_mode(0o600))?;
    Ok(destination)
}

/// Run `<binary> login --with-api-key` with the key on stdin and output
/// discarded. `Ok(None)` is a signal death; a timeout is an error.
fn run_login(
    binary: &str,
    env: &IndexMap<String, String>,
    key: &str,
    timeout: Duration,
) -> io::Result<Option<i32>> {
    let mut proc = Command::new(binary)
        .args(["login", "--with-api-key"])
        .env_clear()
        .envs(env)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    if let Some(mut stdin) = proc.stdin.take() {
        // Like communicate(): a child that exits without reading is not an error.
        let _ = stdin.write_all(format!("{key}\n").as_bytes());
    }
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = proc.try_wait()? {
            return Ok(status.code());
        }
        if Instant::now() >= deadline {
            let _ = proc.kill();
            let _ = proc.wait();
            return Err(io::Error::new(io::ErrorKind::TimedOut, "codex login timed out"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests;
