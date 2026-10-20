//! Install / uninstall eve-bridge hooks into Claude Code's settings.json. Port of `eve_bridge/installer.py`.
//!
//! We identify our entries by a marker substring in the command string (`eve_bridge.hooks.`). Non-eve-bridge
//! hooks are left alone.
//!
//! Which settings.json is the load-bearing question: Claude Code reads its config from `$CLAUDE_CONFIG_DIR` when
//! that is set and from `~/.claude` otherwise. Wrappers (era-code, and anything else running Claude Code against
//! a private config home) set it. Installing into the wrong home fails *silently* — the hooks exist, the daemon
//! runs, the board connects, and no session ever prompts, because the sessions you're actually running never read
//! that file.
//!
//! Every entry point has an `_in` form that takes the environment ([`HomeEnv`]), the hook command
//! ([`HookLaunch`]) and where its lines go ([`Console`]) explicitly, so tests never touch the process.

mod pyjson;

use std::ffi::OsStr;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context};
use eve_claude_home::{settings_path_in, HomeEnv};
use eve_service::Service;
use serde_json::{Map, Value};

pub use pyjson::{dumps_indent2, py_float_repr, py_repr, py_str, py_type_name};

/// The substring that marks a command as one of ours.
pub const MARKER: &str = "eve_bridge.hooks.";

/// The package's name before the rename to eve (2026-10-10). Its entries point at a module that no longer exists,
/// so Claude Code's hooks fail and eve never hears of a session; install replaces them and uninstall removes them.
pub const LEGACY_MARKERS: &[&str] = &[concat!("cc_bu", "ddy_bridge.hooks.")];

/// Must be > the pretooluse hook's `BLOCK_TIMEOUT_SECS` (320 s) + a small buffer.
pub const HOOK_TIMEOUT_SECS: i64 = 330;

/// The timeout of a hook that only reports (needs no decision).
const NOTIFY_TIMEOUT_SECS: i64 = 10;

/// (Claude Code hook event name, python module, matcher, needs_decision)
pub type HookDef = (&'static str, &'static str, Option<&'static str>, bool);

/// Every hook eve installs, in install order.
pub const HOOK_DEFS: &[HookDef] = &[
    ("PreToolUse", "eve_bridge.hooks.pretooluse", Some("Bash"), true),
    ("PreToolUse", "eve_bridge.hooks.pretooluse", Some("Read"), true),
    // A question for the owner: relayed to the phone with numbered options, never answered by a hook.
    ("PreToolUse", "eve_bridge.hooks.pretooluse", Some("AskUserQuestion"), true),
    // Every permission dialog, whatever the tool: a yes/no on the phone while "claude on".
    ("PermissionRequest", "eve_bridge.hooks.permission_request", Some("*"), true),
    ("PostToolUse", "eve_bridge.hooks.posttooluse", Some("*"), false),
    ("SessionStart", "eve_bridge.hooks.session_start", None, false),
    ("SessionEnd", "eve_bridge.hooks.session_end", None, false),
    ("UserPromptSubmit", "eve_bridge.hooks.user_prompt_submit", None, false),
    ("Stop", "eve_bridge.hooks.stop", None, false),
    ("Notification", "eve_bridge.hooks.notification", None, false),
];

// ---------------------------------------------------------------------------------------------------------------
// Where lines go.

/// `print(line)` and `print(line, file=sys.stderr)`.
pub trait Console {
    fn print(&mut self, line: &str);
    fn eprint(&mut self, line: &str);
}

/// This process's stdout and stderr.
#[derive(Debug, Default, Clone, Copy)]
pub struct StdConsole;

impl Console for StdConsole {
    fn print(&mut self, line: &str) {
        let mut out = io::stdout().lock();
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    }

    fn eprint(&mut self, line: &str) {
        let _ = writeln!(io::stderr().lock(), "{line}");
    }
}

/// Lines kept in memory, as pytest's `capsys` reads them.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Captured {
    pub out: String,
    pub err: String,
}

impl Console for Captured {
    fn print(&mut self, line: &str) {
        self.out.push_str(line);
        self.out.push('\n');
    }

    fn eprint(&mut self, line: &str) {
        self.err.push_str(line);
        self.err.push('\n');
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The hook command.

/// What a hook entry runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookLaunch {
    /// `<python> -I -m eve_bridge.hooks.<name>`: Python's line, byte for byte. The string is `sys.executable` of
    /// the interpreter the hooks should run under.
    Python(String),
    /// The Rust hook binaries (`eve-hook-<name>`, from the eve-hooks crate) in this folder. An empty folder means
    /// bare names, found on `PATH`.
    Native(PathBuf),
}

impl HookLaunch {
    /// The hook binaries next to the binary running this install, as Python names the interpreter running it.
    pub fn current() -> HookLaunch {
        let dir = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf))
            .unwrap_or_default();
        HookLaunch::Native(dir)
    }

    /// `_hook_command(module)` for this launch on this build's platform.
    pub fn command(&self, module: &str) -> String {
        self.command_for(module, eve_service::sys_platform())
    }

    /// `_hook_command(module)` as it reads on `platform` (a `sys.platform` value).
    pub fn command_for(&self, module: &str, platform: &str) -> String {
        match self {
            HookLaunch::Python(exe) => hook_command(&python_executable(exe, platform), module),
            HookLaunch::Native(dir) => {
                let bin = native_binary(module);
                let path = if dir.as_os_str().is_empty() { PathBuf::from(&bin) } else { dir.join(&bin) };
                let mut path = path.to_string_lossy().into_owned();
                if platform == "win32" {
                    path = path.replace('\\', "/");
                }
                // The trailing shell comment carries the module name, so the marker identifies this entry exactly
                // as it identifies Python's: either daemon's install replaces the other's entry for the same
                // module, and either daemon's uninstall removes it. Claude Code runs the command through a shell,
                // which drops the comment.
                format!("{} # {module}", shlex_quote(&path))
            }
        }
    }
}

/// `_python_executable`: the interpreter path as it goes into the command. On Windows, backslashes become forward
/// slashes for bash compatibility.
pub fn python_executable(exe: &str, platform: &str) -> String {
    if platform == "win32" {
        exe.replace('\\', "/")
    } else {
        exe.to_string()
    }
}

/// `_hook_command`: `-I` (isolated). Claude Code runs a hook in the session's folder, and plain `-m` puts that
/// folder first on `sys.path`, so a `json.py` planted there ran as the hook (shown 2026-10-12). Isolated mode
/// leaves the folder and the PYTHON* variables out; the venv's own packages still load.
pub fn hook_command(python: &str, module: &str) -> String {
    format!("{python} -I -m {module}")
}

/// The eve-hooks binary that ports `module`: `eve_bridge.hooks.session_start` is `eve-hook-session-start`.
pub fn native_binary(module: &str) -> String {
    let name = module.strip_prefix(MARKER).unwrap_or(module);
    format!("eve-hook-{}", name.replace('_', "-"))
}

/// `shlex.quote`.
fn shlex_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    let safe = s.chars().all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c));
    if safe {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\"'\"'"))
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Python semantics over JSON values.

/// `obj.get(key, default)` where `obj` must be a dict: anything else is Python's `AttributeError`.
fn dict_get<'a>(obj: &'a Value, key: &str) -> anyhow::Result<Option<&'a Value>> {
    match obj {
        Value::Object(m) => Ok(m.get(key)),
        other => Err(no_attribute(other, "get")),
    }
}

fn no_attribute(v: &Value, attr: &str) -> anyhow::Error {
    anyhow!("AttributeError: '{}' object has no attribute '{attr}'", py_type_name(v))
}

/// Python truthiness of a JSON value.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(m) => !m.is_empty(),
    }
}

/// The command of a hook entry when it is a string; Err when the entry is not a dict.
fn command_str(hook_obj: &Value) -> anyhow::Result<Option<&str>> {
    Ok(dict_get(hook_obj, "command")?.and_then(Value::as_str))
}

/// `_is_our_entry`: the command names one of our hook modules, under the current name or the old one.
pub fn is_our_entry(hook_obj: &Value) -> anyhow::Result<bool> {
    Ok(command_str(hook_obj)?.is_some_and(|cmd| cmd.contains(MARKER) || LEGACY_MARKERS.iter().any(|m| cmd.contains(m))))
}

/// `_is_legacy`: the command names a hook module under the name from before the rename.
pub fn is_legacy(hook_obj: &Value) -> anyhow::Result<bool> {
    Ok(command_str(hook_obj)?.is_some_and(|cmd| LEGACY_MARKERS.iter().any(|m| cmd.contains(m))))
}

/// `_find_matcher_group`: the group in `entries` for `matcher`. Absent matcher only matches a group with no
/// `matcher` key. Returns the index.
pub fn find_matcher_group(entries: &[Value], matcher: Option<&str>) -> Option<usize> {
    entries.iter().position(|e| match (e, matcher) {
        (Value::Object(m), None) => !m.contains_key("matcher"),
        (Value::Object(m), Some(want)) => m.get("matcher").and_then(Value::as_str) == Some(want),
        _ => false,
    })
}

// ---------------------------------------------------------------------------------------------------------------
// The settings file.

/// `_load_settings`: `{}` when the file is missing, else the parsed JSON.
pub fn load_settings(path: &Path) -> anyhow::Result<Value> {
    if !path.exists() {
        return Ok(Value::Object(Map::new()));
    }
    let text = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_slice(&text).with_context(|| format!("parsing {}", path.display()))
}

/// `_save_settings`: `json.dump(data, indent=2, ensure_ascii=False)` and a newline, written in place (the file is
/// truncated, never replaced, so a symlinked settings.json stays a symlink).
pub fn save_settings(path: &Path, data: &Value) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
    }
    let mut text = dumps_indent2(data);
    text.push('\n');
    fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}

/// `_backup`: copy `path` beside itself as `settings.json.ccbb-backup-<local time>`, with its permissions and
/// times (`shutil.copy2`).
pub fn backup(path: &Path) -> anyhow::Result<PathBuf> {
    let ts = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let dest = path.with_file_name(format!("settings.json.ccbb-backup-{ts}"));
    fs::copy(path, &dest).with_context(|| format!("copying {} to {}", path.display(), dest.display()))?;
    let meta = fs::metadata(path)?;
    let mut times = fs::FileTimes::new();
    if let Ok(t) = meta.accessed() {
        times = times.set_accessed(t);
    }
    if let Ok(t) = meta.modified() {
        times = times.set_modified(t);
    }
    fs::OpenOptions::new().write(true).open(&dest)?.set_times(times)?;
    Ok(dest)
}

// ---------------------------------------------------------------------------------------------------------------
// install

/// `install_hooks(config_dir)` for this process: its environment, the hook binaries next to this one, stdout.
pub fn install_hooks(config_dir: Option<&OsStr>) -> anyhow::Result<i32> {
    install_hooks_in(&HomeEnv::from_process(), config_dir, &HookLaunch::current(), &mut StdConsole)
}

/// `install_hooks` against an explicit environment, hook command and console.
pub fn install_hooks_in(
    env: &HomeEnv,
    config_dir: Option<&OsStr>,
    launch: &HookLaunch,
    console: &mut dyn Console,
) -> anyhow::Result<i32> {
    let path = settings_path_in(env, config_dir)?;
    install_hooks_at(&path, launch, console)
}

/// `install_hooks` into the settings file at `path`.
pub fn install_hooks_at(path: &Path, launch: &HookLaunch, console: &mut dyn Console) -> anyhow::Result<i32> {
    // The config HOME must exist — that proves Claude Code actually uses it, and guards against a typo'd
    // --config-dir silently creating a tree nothing reads. The local settings file itself is ours to create: it is
    // the file wrappers do not own (see claude_home.settings_path) and often won't exist.
    let home = path.parent().unwrap_or(Path::new(""));
    let home_shown = if home.as_os_str().is_empty() { Path::new(".") } else { home };
    if !home_shown.is_dir() {
        console.eprint(&format!("Claude config home not found at {}", home_shown.display()));
        return Ok(2);
    }
    if !path.exists() {
        fs::write(path, "{}\n").with_context(|| format!("writing {}", path.display()))?;
        console.print(&format!("created {}", path.display()));
    } else {
        let dest = backup(path)?;
        console.print(&format!("backed up settings to {}", dest.display()));
    }

    let mut data = load_settings(path)?;
    let added = merge_hooks(&mut data, launch, console)?;

    save_settings(path, &data)?;
    console.print(&format!("installed {added} hook(s) into {}", path.display()));
    if added == 0 {
        console.print("(already up to date)");
    }
    Ok(0)
}

/// The body of `install_hooks` over the parsed settings: drop the entries from before the rename, then add each
/// of [`HOOK_DEFS`] that is not already there exactly. Returns how many were added.
pub fn merge_hooks(data: &mut Value, launch: &HookLaunch, console: &mut dyn Console) -> anyhow::Result<usize> {
    let Value::Object(root) = data else {
        return Err(no_attribute(data, "setdefault"));
    };
    let hooks = root.entry("hooks").or_insert_with(|| Value::Object(Map::new()));
    let Value::Object(hooks) = hooks else {
        return Err(no_attribute(hooks, "values"));
    };


    let mut removed = 0usize;
    for entries in hooks.values_mut() {
        let Value::Array(groups) = entries else { continue };
        for group in groups.iter_mut() {
            let Value::Object(g) = group else { continue };
            let Some(Value::Array(inner)) = g.get_mut("hooks") else { continue };
            let before = inner.len();
            let mut kept = Vec::with_capacity(before);
            for h in inner.iter() {
                if !is_legacy(h)? {
                    kept.push(h.clone());
                }
            }
            removed += before - kept.len();
            *inner = kept;
        }
    }
    for entries in hooks.values_mut() {
        if let Value::Array(groups) = entries {
            groups.retain(|g| !matches!(g, Value::Object(m) if m.get("hooks") == Some(&Value::Array(Vec::new()))));
        }
    }
    if removed > 0 {
        console.print(&format!("removed {removed} hook(s) left from before the rename"));
    }

    let mut added = 0usize;
    for &(event, module, matcher, needs_decision) in HOOK_DEFS {
        let entries = hooks.entry(event).or_insert_with(|| Value::Array(Vec::new()));
        let cmd = launch.command(module);
        let entries = match entries {
            Value::Array(a) => a,
            Value::String(_) | Value::Object(_) => return Err(no_attribute(entries, "append")),
            other => return Err(not_iterable(other)),
        };

        // Find or create the matcher group.
        let idx = match find_matcher_group(entries, matcher) {
            Some(i) => i,
            None => {
                let mut group = Map::new();
                group.insert("hooks".into(), Value::Array(Vec::new()));
                if let Some(m) = matcher {
                    group.insert("matcher".into(), Value::String(m.into()));
                }
                entries.push(Value::Object(group));
                entries.len() - 1
            }
        };
        let Value::Object(group) = &mut entries[idx] else {
            unreachable!("find_matcher_group only returns dict groups")
        };

        let inner = group.entry("hooks").or_insert_with(|| Value::Array(Vec::new()));
        let inner = match inner {
            Value::Array(a) => a,
            Value::Null | Value::Bool(_) | Value::Number(_) => return Err(not_iterable(inner)),
            Value::String(s) if !s.is_empty() => return Err(no_attribute(&Value::String(String::new()), "get")),
            Value::Object(m) if !m.is_empty() => return Err(no_attribute(&Value::String(String::new()), "get")),
            // An empty dict takes `inner[:] = [...]` as a new key (slices are hashable from Python 3.12, and the
            // daemon's venv runs 3.13), then has no `append`.
            Value::Object(_) => return Err(no_attribute(inner, "append")),
            other => {
                return Err(anyhow!(
                    "TypeError: '{}' object does not support item assignment",
                    py_type_name(other)
                ))
            }
        };
        // Skip if an identical eve-bridge entry already exists.
        let mut present = false;
        for h in inner.iter() {
            if is_our_entry(h)? && h.get("command").and_then(Value::as_str) == Some(cmd.as_str()) {
                present = true;
                break;
            }
        }
        if present {
            continue;
        }

        // Remove any stale eve-bridge entry for the same module (path changed, etc.).
        let mut kept = Vec::with_capacity(inner.len() + 1);
        for h in inner.iter() {
            let stale = is_our_entry(h)? && command_str(h)?.is_some_and(|c| c.contains(module));
            if !stale {
                kept.push(h.clone());
            }
        }
        *inner = kept;
        let mut entry = Map::new();
        entry.insert("type".into(), Value::String("command".into()));
        entry.insert("command".into(), Value::String(cmd));
        entry.insert(
            "timeout".into(),
            Value::from(if needs_decision { HOOK_TIMEOUT_SECS } else { NOTIFY_TIMEOUT_SECS }),
        );
        inner.push(Value::Object(entry));
        added += 1;
    }
    Ok(added)
}

fn not_iterable(v: &Value) -> anyhow::Error {
    anyhow!("TypeError: '{}' object is not iterable", py_type_name(v))
}

// ---------------------------------------------------------------------------------------------------------------
// uninstall

/// `uninstall_hooks(config_dir)` for this process.
pub fn uninstall_hooks(config_dir: Option<&OsStr>) -> anyhow::Result<i32> {
    uninstall_hooks_in(&HomeEnv::from_process(), config_dir, &mut StdConsole)
}

/// `uninstall_hooks` against an explicit environment and console.
pub fn uninstall_hooks_in(env: &HomeEnv, config_dir: Option<&OsStr>, console: &mut dyn Console) -> anyhow::Result<i32> {
    let path = settings_path_in(env, config_dir)?;
    uninstall_hooks_at(&path, console)
}

/// `uninstall_hooks` from the settings file at `path`.
pub fn uninstall_hooks_at(path: &Path, console: &mut dyn Console) -> anyhow::Result<i32> {
    if !path.exists() {
        console.eprint(&format!("settings.json not found at {}", path.display()));
        return Ok(2);
    }

    let mut data = load_settings(path)?;
    let Some(removed) = strip_hooks(&mut data)? else {
        console.print("no hooks block — nothing to remove");
        return Ok(0);
    };

    if removed == 0 {
        console.print("no eve-bridge hooks found — nothing to remove");
        return Ok(0);
    }

    let dest = backup(path)?;
    console.print(&format!("backed up settings to {}", dest.display()));
    save_settings(path, &data)?;
    console.print(&format!("removed {removed} hook(s) from {}", path.display()));
    Ok(0)
}

/// The body of `uninstall_hooks` over the parsed settings: remove every entry of ours, drop the groups and events
/// left empty, and the `hooks` block when nothing is left in it. None when there is no `hooks` dict at all;
/// otherwise how many entries were removed.
pub fn strip_hooks(data: &mut Value) -> anyhow::Result<Option<usize>> {
    let Value::Object(root) = data else {
        return Err(no_attribute(data, "get"));
    };
    let Some(Value::Object(hooks)) = root.get_mut("hooks") else {
        return Ok(None);
    };

    let mut removed = 0usize;
    let events: Vec<String> = hooks.keys().cloned().collect();
    for event in events {
        let value = hooks.get(&event).cloned().unwrap_or(Value::Null);
        // `hooks.get(event) or []`: any falsy value counts as an empty list.
        let entries = if truthy(&value) { value } else { Value::Array(Vec::new()) };
        let Value::Array(entries) = entries else { continue };
        let mut cleaned_groups = Vec::with_capacity(entries.len());
        for mut group in entries {
            let Value::Object(g) = &mut group else {
                cleaned_groups.push(group);
                continue;
            };
            let raw = g.get("hooks").cloned().unwrap_or(Value::Null);
            let inner = if truthy(&raw) { raw } else { Value::Array(Vec::new()) };
            let inner = match inner {
                Value::Array(a) => a,
                Value::Bool(_) | Value::Number(_) => {
                    return Err(anyhow!("TypeError: object of type '{}' has no len()", py_type_name(&inner)))
                }
                // A non-empty str or dict: its characters or keys are iterated as entries, and a str has no `get`.
                _ => return Err(no_attribute(&Value::String(String::new()), "get")),
            };
            let before = inner.len();
            let mut remaining = Vec::with_capacity(before);
            for h in inner {
                if !is_our_entry(&h)? {
                    remaining.push(h);
                }
            }
            removed += before - remaining.len();
            if !remaining.is_empty() {
                g.insert("hooks".into(), Value::Array(remaining));
                cleaned_groups.push(group);
            }
            // else drop the group entirely
        }
        if cleaned_groups.is_empty() {
            hooks.shift_remove(&event);
        } else {
            hooks.insert(event, Value::Array(cleaned_groups));
        }
    }

    if hooks.is_empty() {
        root.shift_remove("hooks");
    }
    Ok(Some(removed))
}

// ---------------------------------------------------------------------------------------------------------------
// status

/// `show_status(config_dir)` for this process: its environment, its service backend, stdout.
pub fn show_status(config_dir: Option<&OsStr>) -> anyhow::Result<i32> {
    show_status_in(&HomeEnv::from_process(), config_dir, || Ok(Service::from_process()?), &mut StdConsole)
}

/// `show_status` against an explicit environment, console and service. `service` is asked for only once the
/// hooks are printed, where Python imports the service module.
pub fn show_status_in(
    env: &HomeEnv,
    config_dir: Option<&OsStr>,
    service: impl FnOnce() -> anyhow::Result<Service>,
    console: &mut dyn Console,
) -> anyhow::Result<i32> {
    let path = settings_path_in(env, config_dir)?;
    console.print(&format!("Hooks: ({})", path.display()));
    if let Some(dir) = env.claude_config_dir.as_deref().filter(|d| !d.is_empty()) {
        console.print(&format!("  CLAUDE_CONFIG_DIR={}", dir.to_string_lossy()));
    }
    if !path.exists() {
        console.print(&format!("  settings.json not found at {}", path.display()));
    } else {
        let data = load_settings(&path)?;
        print_installed(&data, console)?;
    }

    let service = service()?;
    let backend = service.backend_name();
    let header = match backend {
        Some(name) => format!("Service ({name}):"),
        None => "Service:".to_string(),
    };
    console.print(&format!("\n{header}"));
    if backend.is_none() {
        console.print(&format!("  unsupported platform ({})", service.platform));
    } else if !service.is_installed()? {
        console.print("  not installed (run `eve-bridge install --service` to add)");
    } else {
        let loaded = if service.is_loaded()? { "loaded" } else { "installed but not loaded" };
        let unit = service.unit_path().map(|l| l.to_string()).unwrap_or_else(|| "None".into());
        let logs = service.log_path().map(|l| l.to_string()).unwrap_or_else(|| "None".into());
        console.print(&format!("  {loaded}: {unit}"));
        console.print(&format!("  logs: {logs}"));
    }
    Ok(0)
}

/// The hook lines of `show_status` for parsed settings: one `  <event> [<matcher>] → <command>` per entry of
/// ours, or `  no eve-bridge hooks installed`. Err where Python raises; the lines before the error are lost here,
/// so `show_status` uses [`print_installed`], which prints them as Python does.
pub fn installed_lines(data: &Value) -> anyhow::Result<Vec<String>> {
    let mut lines = Captured::default();
    print_installed(data, &mut lines)?;
    Ok(lines.out.lines().map(str::to_string).collect())
}

/// The hook lines of `show_status`, printed one by one as Python prints them: on malformed settings the lines
/// found before the bad entry are already out when the error comes back.
pub fn print_installed(data: &Value, console: &mut dyn Console) -> anyhow::Result<()> {
    let raw = dict_get(data, "hooks")?.cloned().unwrap_or(Value::Null);
    let hooks = if truthy(&raw) { raw } else { Value::Object(Map::new()) };
    let Value::Object(hooks) = hooks else {
        return Err(no_attribute(&hooks, "items"));
    };
    let mut any_installed = false;
    for (event, entries) in &hooks {
        let Value::Array(entries) = entries else { continue };
        for group in entries {
            let Value::Object(g) = group else { continue };
            let raw = g.get("hooks").cloned().unwrap_or(Value::Null);
            let inner = if truthy(&raw) { raw } else { Value::Array(Vec::new()) };
            let inner = match inner {
                Value::Array(a) => a,
                Value::Bool(_) | Value::Number(_) => return Err(not_iterable(&inner)),
                _ => return Err(no_attribute(&Value::String(String::new()), "get")),
            };
            for h in &inner {
                if is_our_entry(h)? {
                    any_installed = true;
                    let matcher = g.get("matcher").map(py_str).unwrap_or_else(|| "*".into());
                    let command = h.get("command").map(py_str).unwrap_or_else(|| "None".into());
                    console.print(&format!("  {event} [{matcher}] → {command}"));
                }
            }
        }
    }
    if !any_installed {
        console.print("  no eve-bridge hooks installed");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
