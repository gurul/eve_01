//! Installer tests — run against a temp settings.json so we never touch the real one.
//!
//! Python's fixture points `$CLAUDE_CONFIG_DIR` at a temp home; here that is a [`HomeEnv`] whose
//! `claude_config_dir` is the temp folder, so the tests still exercise the config-home resolution (the thing that
//! regressed in production: hooks silently installed into a home no session reads) without touching the process
//! environment.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use eve_service::{Backend, Completed, FullPaths, Host, Launch, Launchd, Service};
use serde_json::{json, Value};
use tempfile::TempDir;

use super::*;

const PY: &str = "/venv/bin/python";
const OLD: &str = concat!("cc_bu", "ddy_bridge");

fn python() -> HookLaunch {
    HookLaunch::Python(PY.into())
}

struct Temp {
    _dir: TempDir,
    home: PathBuf,
    env: HomeEnv,
}

impl Temp {
    fn settings(&self) -> PathBuf {
        self.home.join("settings.json")
    }
}

/// `temp_settings`: `$CLAUDE_CONFIG_DIR` at a temp home.
fn temp_settings() -> Temp {
    let dir = TempDir::new().unwrap();
    let home = dir.path().to_path_buf();
    let env = HomeEnv {
        claude_config_dir: Some(home.clone().into_os_string()),
        claude_config_dirs: None,
        home: Some(OsString::from("/nonexistent-home")),
    };
    Temp { _dir: dir, home, env }
}

fn baseline(extra: Option<Value>) -> Value {
    let mut d = json!({
        "statusLine": {"type": "command", "command": "true"},
        "permissions": {"defaultMode": "auto"},
    });
    if let Some(Value::Object(extra)) = extra {
        for (k, v) in extra {
            d.as_object_mut().unwrap().insert(k, v);
        }
    }
    d
}

fn write(p: &Path, data: &Value) {
    std::fs::write(p, serde_json::to_string_pretty(data).unwrap()).unwrap();
}

fn read(p: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn install(t: &Temp) -> (i32, Captured) {
    let mut c = Captured::default();
    let rc = install_hooks_in(&t.env, None, &python(), &mut c).unwrap();
    (rc, c)
}

fn uninstall(t: &Temp) -> (i32, Captured) {
    let mut c = Captured::default();
    let rc = uninstall_hooks_in(&t.env, None, &mut c).unwrap();
    (rc, c)
}

fn backups(home: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(home)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with("settings.json.ccbb-backup-"))
        .collect();
    v.sort();
    v
}

// ---- bridge/tests/test_installer.py -----------------------------------------------------------------------------

#[test]
fn test_install_from_scratch() {
    let t = temp_settings();
    write(&t.settings(), &baseline(None));
    assert_eq!(install(&t).0, 0);
    let data = read(&t.settings());
    assert!(data.get("hooks").is_some());
    // All hook events covered.
    let mut keys: Vec<&str> = data["hooks"].as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort();
    let mut want = vec![
        "PreToolUse",
        "PostToolUse",
        "SessionStart",
        "SessionEnd",
        "UserPromptSubmit",
        "Stop",
        "Notification",
        "PermissionRequest",
    ];
    want.sort();
    assert_eq!(keys, want);
    // Non-hook settings preserved.
    assert_eq!(data["statusLine"]["command"], "true");
    assert_eq!(data["permissions"]["defaultMode"], "auto");
}

#[test]
fn test_install_is_idempotent() {
    let t = temp_settings();
    write(&t.settings(), &baseline(None));
    install(&t);
    let first = read(&t.settings());
    let (_, out) = install(&t);
    let second = read(&t.settings());
    // Same number of entries — no duplicates.
    let n1 = first["hooks"]["PreToolUse"][0]["hooks"].as_array().unwrap().len();
    let n2 = second["hooks"]["PreToolUse"][0]["hooks"].as_array().unwrap().len();
    assert_eq!(n1, 1);
    assert_eq!(n2, 1);
    assert_eq!(first, second);
    assert!(out.out.ends_with(&format!("installed 0 hook(s) into {}\n(already up to date)\n", t.settings().display())));
}

#[test]
fn test_uninstall_removes_only_our_entries() {
    let t = temp_settings();
    let baseline = baseline(Some(json!({
        "hooks": {
            "PreToolUse": [
                {
                    "matcher": "Bash",
                    "hooks": [
                        {"type": "command", "command": "/path/to/unrelated-hook", "timeout": 5},
                    ],
                }
            ]
        }
    })));
    write(&t.settings(), &baseline);
    install(&t);

    // Now install added our Bash hook alongside the user's.
    let after_install = read(&t.settings());
    let bash_group = &after_install["hooks"]["PreToolUse"][0];
    assert_eq!(bash_group["matcher"], "Bash");
    assert_eq!(bash_group["hooks"].as_array().unwrap().len(), 2);

    assert_eq!(uninstall(&t).0, 0);
    let after_uninstall = read(&t.settings());
    // User's unrelated hook survived.
    let bash_group = &after_uninstall["hooks"]["PreToolUse"][0];
    assert_eq!(bash_group["hooks"].as_array().unwrap().len(), 1);
    assert_eq!(bash_group["hooks"][0]["command"], "/path/to/unrelated-hook");
}

#[test]
fn test_uninstall_drops_empty_hooks_block() {
    let t = temp_settings();
    write(&t.settings(), &baseline(None));
    install(&t);
    uninstall(&t);
    let data = read(&t.settings());
    // Nothing else was in hooks; block should be gone.
    assert!(data.get("hooks").is_none());
}

#[test]
fn test_uninstall_when_nothing_to_remove() {
    let t = temp_settings();
    write(&t.settings(), &baseline(None));
    let (rc, out) = uninstall(&t); // no-op, exit clean
    assert_eq!(rc, 0);
    assert_eq!(out.out, "no hooks block — nothing to remove\n");
}

#[test]
fn test_install_replaces_the_hooks_from_before_the_rename() {
    // 2026-10-10: after the rename the old entries stayed, every hook failed, and "claude on" found no session.
    let t = temp_settings();
    let old = format!("/old/venv/bin/python -m {OLD}.hooks.");
    let foreign = json!({"type": "command", "command": "other-tool hook"});
    write(
        &t.settings(),
        &baseline(Some(json!({"hooks": {
            "SessionStart": [{"hooks": [{"type": "command", "command": format!("{old}session_start")}, foreign]}],
            "Stop": [{"hooks": [{"type": "command", "command": format!("{old}stop")}]}],
        }}))),
    );
    let (rc, out) = install(&t);
    assert_eq!(rc, 0);
    assert!(out.out.contains("removed 2 hook(s) left from before the rename\n"));
    let text = std::fs::read_to_string(t.settings()).unwrap();
    assert!(!text.contains("ddy_bridge") && text.contains("other-tool hook"));
    let data: Value = serde_json::from_str(&text).unwrap();
    let starts: Vec<&str> = data["hooks"]["SessionStart"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|g| g["hooks"].as_array().unwrap())
        .map(|h| h["command"].as_str().unwrap())
        .collect();
    assert!(starts.iter().any(|c| c.ends_with("-m eve_bridge.hooks.session_start")));
    // no empty groups left behind
    for e in data["hooks"].as_object().unwrap().values() {
        for g in e.as_array().unwrap() {
            assert!(!g["hooks"].as_array().unwrap().is_empty());
        }
    }
}

/// A hook runs in the session's folder; with plain `-m` a planted json.py there ran instead of the stdlib's.
#[test]
fn test_hooks_run_isolated_so_the_session_folder_is_not_on_the_import_path() {
    let cmd = python().command("eve_bridge.hooks.stop");
    assert!(cmd.contains(" -I -m eve_bridge.hooks.stop"));
    assert_eq!(cmd, "/venv/bin/python -I -m eve_bridge.hooks.stop");

    let Ok(py) = which_python() else {
        eprintln!("python3 not on PATH: the isolation probe needs an interpreter");
        return;
    };
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("json.py"), "print('HIJACKED')\n").unwrap();
    let probe = "import json, sys; print('ok' if 'HIJACKED' not in open(json.__file__).read() else 'bad')";
    let isolated = std::process::Command::new(&py).args(["-I", "-c", probe]).current_dir(tmp.path()).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&isolated.stdout).trim(), "ok");
    let plain =
        std::process::Command::new(&py).args(["-m", "json.tool", "--help"]).current_dir(tmp.path()).output().unwrap();
    // positive control: the hole is real without -I
    assert!(String::from_utf8_lossy(&plain.stdout).contains("HIJACKED"));
}

fn which_python() -> Result<PathBuf, ()> {
    std::env::var_os("PATH")
        .and_then(|paths| std::env::split_paths(&paths).map(|d| d.join("python3")).find(|p| p.is_file()))
        .ok_or(())
}

// ---- bridge/tests/test_relay_questions.py (the installer's part) -------------------------------------------------

#[test]
fn test_installer_registers_the_question_and_every_dialog() {
    assert!(HOOK_DEFS.contains(&("PreToolUse", "eve_bridge.hooks.pretooluse", Some("AskUserQuestion"), true)));
    assert!(HOOK_DEFS.contains(&("PermissionRequest", "eve_bridge.hooks.permission_request", Some("*"), true)));
}

// ---- byte parity with the Python installer -----------------------------------------------------------------------

fn fixture() -> String {
    include_str!("../tests/data/settings_before.json").replace("OLD_PACKAGE", OLD)
}

/// The fixture (foreign hooks, a stale entry of ours, entries from before the rename, an empty group, a non-list
/// event, floats, non-ASCII and control characters) installed by the Python module with
/// `sys.executable = "/venv/bin/python"`, then uninstalled: the Rust installer writes the same bytes.
#[test]
fn install_and_uninstall_write_the_bytes_python_writes() {
    let t = temp_settings();
    std::fs::write(t.settings(), fixture()).unwrap();
    let (rc, out) = install(&t);
    assert_eq!(rc, 0);
    let installed = std::fs::read_to_string(t.settings()).unwrap();
    assert_eq!(installed, include_str!("../tests/data/settings_installed_by_python.json"));
    let backup = backups(&t.home);
    assert_eq!(backup.len(), 1);
    assert_eq!(std::fs::read_to_string(&backup[0]).unwrap(), fixture());
    assert_eq!(
        out.out,
        format!(
            "backed up settings to {}\nremoved 2 hook(s) left from before the rename\ninstalled 10 hook(s) into {}\n",
            backup[0].display(),
            t.settings().display()
        )
    );
    assert_eq!(out.err, "");

    for b in backup {
        std::fs::remove_file(b).unwrap();
    }
    let (rc, out) = uninstall(&t);
    assert_eq!(rc, 0);
    assert_eq!(
        std::fs::read_to_string(t.settings()).unwrap(),
        include_str!("../tests/data/settings_uninstalled_by_python.json")
    );
    let backup = backups(&t.home);
    assert_eq!(backup.len(), 1);
    assert_eq!(std::fs::read_to_string(&backup[0]).unwrap(), installed);
    assert_eq!(
        out.out,
        format!("backed up settings to {}\nremoved 10 hook(s) from {}\n", backup[0].display(), t.settings().display())
    );
}

#[test]
fn floats_are_written_as_python_repr() {
    for (f, want) in [
        (1.5, "1.5"),
        (1e-05, "1e-05"),
        (1e16, "1e+16"),
        (1e15, "1000000000000000.0"),
        (100.0, "100.0"),
        (0.0001, "0.0001"),
        (-0.0, "-0.0"),
        (0.0, "0.0"),
        (2.5e-7, "2.5e-07"),
        (1e300, "1e+300"),
        (-1.2345e-120, "-1.2345e-120"),
        (123456789.125, "123456789.125"),
        (0.1 + 0.2, "0.30000000000000004"),
        (f64::NAN, "NaN"),
        (f64::INFINITY, "Infinity"),
        (f64::NEG_INFINITY, "-Infinity"),
    ] {
        assert_eq!(py_float_repr(f), want, "{f:?}");
    }
    assert_eq!(dumps_indent2(&json!({"a": [], "b": {}, "c": [1, 2.0]})), "{\n  \"a\": [],\n  \"b\": {},\n  \"c\": [\n    1,\n    2.0\n  ]\n}");
}

// ---- the error paths ---------------------------------------------------------------------------------------------

#[test]
fn install_refuses_a_config_home_that_does_not_exist() {
    let t = temp_settings();
    let missing = t.home.join("typo");
    let mut c = Captured::default();
    let rc = install_hooks_in(&t.env, Some(missing.as_os_str()), &python(), &mut c).unwrap();
    assert_eq!(rc, 2);
    assert_eq!(c.err, format!("Claude config home not found at {}\n", missing.display()));
    assert!(!missing.exists());
}

#[test]
fn install_creates_the_settings_file_when_the_home_has_none() {
    let t = temp_settings();
    let (rc, out) = install(&t);
    assert_eq!(rc, 0);
    assert!(out.out.starts_with(&format!("created {}\n", t.settings().display())));
    assert!(out.out.contains("installed 10 hook(s) into "));
    assert!(backups(&t.home).is_empty());
    let data = read(&t.settings());
    assert_eq!(data.as_object().unwrap().keys().collect::<Vec<_>>(), vec!["hooks"]);
}

#[test]
fn explicit_config_dir_wins_over_the_environment() {
    let t = temp_settings();
    let other = TempDir::new().unwrap();
    let mut c = Captured::default();
    assert_eq!(install_hooks_in(&t.env, Some(other.path().as_os_str()), &python(), &mut c).unwrap(), 0);
    assert!(other.path().join("settings.json").exists());
    assert!(!t.settings().exists());
}

#[test]
fn entries_get_the_decision_timeout_and_matchers_as_defined() {
    let t = temp_settings();
    install(&t);
    let data = read(&t.settings());
    for &(event, module, matcher, needs_decision) in HOOK_DEFS {
        let groups = data["hooks"][event].as_array().unwrap();
        let group = groups
            .iter()
            .find(|g| g.get("matcher").and_then(Value::as_str) == matcher)
            .unwrap_or_else(|| panic!("{event} {matcher:?}"));
        let entry = &group["hooks"][0];
        assert_eq!(entry["type"], "command");
        assert_eq!(entry["command"], format!("{PY} -I -m {module}"));
        assert_eq!(entry["timeout"], if needs_decision { 330 } else { 10 });
        // New groups put "hooks" first, then the matcher, as Python's dict does.
        let keys: Vec<&String> = group.as_object().unwrap().keys().collect();
        match matcher {
            Some(_) => assert_eq!(keys, ["hooks", "matcher"]),
            None => assert_eq!(keys, ["hooks"]),
        }
    }
}

#[test]
fn a_hook_entry_that_is_not_a_dict_raises_as_python_does() {
    let t = temp_settings();
    write(&t.settings(), &json!({"hooks": {"Stop": [{"hooks": ["not a dict"]}]}}));
    let mut c = Captured::default();
    let err = install_hooks_in(&t.env, None, &python(), &mut c).unwrap_err();
    assert_eq!(err.to_string(), "AttributeError: 'str' object has no attribute 'get'");
    let err = uninstall_hooks_in(&t.env, None, &mut c).unwrap_err();
    assert_eq!(err.to_string(), "AttributeError: 'str' object has no attribute 'get'");

    write(&t.settings(), &json!([1, 2]));
    let err = install_hooks_in(&t.env, None, &python(), &mut c).unwrap_err();
    assert_eq!(err.to_string(), "AttributeError: 'list' object has no attribute 'setdefault'");

    write(&t.settings(), &json!({"hooks": {"Stop": null}}));
    let err = install_hooks_in(&t.env, None, &python(), &mut c).unwrap_err();
    assert_eq!(err.to_string(), "TypeError: 'NoneType' object is not iterable");

    // Python 3.13 (the daemon's venv): `{}[:] = []` adds a key, then `{}.append` fails.
    write(&t.settings(), &json!({"hooks": {"Stop": [{"hooks": {}}]}}));
    let err = install_hooks_in(&t.env, None, &python(), &mut c).unwrap_err();
    assert_eq!(err.to_string(), "AttributeError: 'dict' object has no attribute 'append'");
    write(&t.settings(), &json!({"hooks": {"Stop": [{"hooks": ""}]}}));
    let err = install_hooks_in(&t.env, None, &python(), &mut c).unwrap_err();
    assert_eq!(err.to_string(), "TypeError: 'str' object does not support item assignment");
}

/// Python prints each status line as it finds it, so the entries before a malformed one are already out when it
/// raises (output taken from the Python module on the same settings).
#[test]
fn status_prints_the_lines_found_before_a_bad_entry() {
    let data = json!({"hooks": {"Stop": [{"hooks": [{"command": "/x/python -m eve_bridge.hooks.stop"}, "junk"]}]}});
    let mut c = Captured::default();
    let err = print_installed(&data, &mut c).unwrap_err();
    assert_eq!(err.to_string(), "AttributeError: 'str' object has no attribute 'get'");
    assert_eq!(c.out, "  Stop [*] → /x/python -m eve_bridge.hooks.stop\n");
    assert!(installed_lines(&data).is_err());
}

#[test]
fn uninstall_without_a_settings_file_returns_2() {
    let t = temp_settings();
    let (rc, out) = uninstall(&t);
    assert_eq!(rc, 2);
    assert_eq!(out.err, format!("settings.json not found at {}\n", t.settings().display()));
}

#[test]
fn uninstall_with_only_foreign_hooks_writes_nothing() {
    let t = temp_settings();
    let before = json!({"hooks": {"Stop": [{"hooks": [{"type": "command", "command": "say done"}]}]}});
    write(&t.settings(), &before);
    let raw = std::fs::read_to_string(t.settings()).unwrap();
    let (rc, out) = uninstall(&t);
    assert_eq!(rc, 0);
    assert_eq!(out.out, "no eve-bridge hooks found — nothing to remove\n");
    assert_eq!(std::fs::read_to_string(t.settings()).unwrap(), raw);
    assert!(backups(&t.home).is_empty());
}

#[test]
fn uninstall_drops_falsy_events_and_groups_without_hooks() {
    // `hooks.get(event) or []` and `group.get("hooks") or []`: an event that is null/empty and a group with no
    // hooks are dropped once something of ours was removed; a group that is not a dict is kept.
    let mut data = json!({"hooks": {
        "A": null,
        "B": [{"matcher": "x"}, "keep me", {"hooks": [{"command": "/p -I -m eve_bridge.hooks.stop"}]}],
        "C": {},
        "D": "text",
    }, "after": 1});
    assert_eq!(strip_hooks(&mut data).unwrap(), Some(1));
    assert_eq!(dumps_indent2(&data), dumps_indent2(&json!({"hooks": {"B": ["keep me"], "D": "text"}, "after": 1})));
    // Order of the remaining keys is kept (a pop, not a swap).
    let mut data = json!({"a": 1, "hooks": {"Stop": [{"hooks": [{"command": "x eve_bridge.hooks.stop"}]}]}, "z": 2});
    assert_eq!(strip_hooks(&mut data).unwrap(), Some(1));
    assert_eq!(data.as_object().unwrap().keys().collect::<Vec<_>>(), ["a", "z"]);
    // No hooks dict at all.
    assert_eq!(strip_hooks(&mut json!({"hooks": []})).unwrap(), None);
}

#[test]
fn find_matcher_group_treats_absent_matcher_only_as_absent() {
    let entries = vec![json!("x"), json!({"matcher": "*", "hooks": []}), json!({"hooks": []}), json!({"matcher": "Bash"})];
    assert_eq!(find_matcher_group(&entries, None), Some(2));
    assert_eq!(find_matcher_group(&entries, Some("*")), Some(1));
    assert_eq!(find_matcher_group(&entries, Some("Bash")), Some(3));
    assert_eq!(find_matcher_group(&entries, Some("Read")), None);
}

#[test]
fn entries_are_ours_by_marker_and_legacy_by_the_old_name() {
    let ours = json!({"command": "/p -I -m eve_bridge.hooks.stop"});
    let legacy = json!({"command": format!("/p -m {OLD}.hooks.stop")});
    let foreign = json!({"command": "say hi"});
    let not_str = json!({"command": ["eve_bridge.hooks.stop"]});
    assert!(is_our_entry(&ours).unwrap() && !is_legacy(&ours).unwrap());
    assert!(is_our_entry(&legacy).unwrap() && is_legacy(&legacy).unwrap());
    assert!(!is_our_entry(&foreign).unwrap() && !is_legacy(&foreign).unwrap());
    assert!(!is_our_entry(&not_str).unwrap());
    assert!(!is_our_entry(&json!({})).unwrap());
}

// ---- the hook command --------------------------------------------------------------------------------------------

#[test]
fn python_executable_uses_forward_slashes_on_windows() {
    assert_eq!(python_executable(r"C:\venv\Scripts\python.exe", "win32"), "C:/venv/Scripts/python.exe");
    assert_eq!(python_executable(r"/odd\path/python", "darwin"), r"/odd\path/python");
    assert_eq!(
        HookLaunch::Python(r"C:\py\python.exe".into()).command_for("eve_bridge.hooks.stop", "win32"),
        "C:/py/python.exe -I -m eve_bridge.hooks.stop"
    );
}

#[test]
fn native_commands_name_the_eve_hooks_binaries_and_carry_the_marker() {
    let launch = HookLaunch::Native(PathBuf::from("/opt/eve/bin"));
    assert_eq!(
        launch.command_for("eve_bridge.hooks.session_start", "darwin"),
        "/opt/eve/bin/eve-hook-session-start # eve_bridge.hooks.session_start"
    );
    let spaced = HookLaunch::Native(PathBuf::from("/Users/a b/it's"));
    assert_eq!(
        spaced.command_for("eve_bridge.hooks.stop", "darwin"),
        "'/Users/a b/it'\"'\"'s/eve-hook-stop' # eve_bridge.hooks.stop"
    );
    assert_eq!(HookLaunch::Native(PathBuf::new()).command_for("eve_bridge.hooks.stop", "linux"), "eve-hook-stop # eve_bridge.hooks.stop");
    assert!(is_our_entry(&json!({"command": launch.command("eve_bridge.hooks.stop")})).unwrap());

    // Every binary named here is one the eve-hooks crate builds.
    let manifest = include_str!("../../eve-hooks/Cargo.toml");
    for &(_, module, _, _) in HOOK_DEFS {
        let bin = native_binary(module);
        assert!(manifest.contains(&format!("name = \"{bin}\"")), "{bin} is not an eve-hooks binary");
    }
}

#[test]
fn native_and_python_installs_replace_each_other() {
    let t = temp_settings();
    install(&t);
    let native = HookLaunch::Native(PathBuf::from("/opt/eve/bin"));
    let mut c = Captured::default();
    install_hooks_in(&t.env, None, &native, &mut c).unwrap();
    assert!(c.out.contains("installed 10 hook(s)"));
    let text = std::fs::read_to_string(t.settings()).unwrap();
    assert!(!text.contains("-I -m"));
    let data: Value = serde_json::from_str(&text).unwrap();
    for groups in data["hooks"].as_object().unwrap().values() {
        for g in groups.as_array().unwrap() {
            assert_eq!(g["hooks"].as_array().unwrap().len(), 1);
        }
    }
    let mut c = Captured::default();
    install_hooks_in(&t.env, None, &native, &mut c).unwrap();
    assert!(c.out.ends_with("(already up to date)\n"));
    install(&t);
    assert!(!std::fs::read_to_string(t.settings()).unwrap().contains("eve-hook-"));
    // And an uninstall removes the native entries as well.
    install_hooks_in(&t.env, None, &native, &mut Captured::default()).unwrap();
    let (_, out) = uninstall(&t);
    assert!(out.out.contains("removed 10 hook(s)"));
}

// ---- backups ----------------------------------------------------------------------------------------------------

#[test]
fn backup_is_named_for_the_local_second_and_keeps_the_times() {
    let t = temp_settings();
    std::fs::write(t.settings(), "{}\n").unwrap();
    let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    std::fs::OpenOptions::new()
        .write(true)
        .open(t.settings())
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(old))
        .unwrap();
    let dest = backup(&t.settings()).unwrap();
    let name = dest.file_name().unwrap().to_string_lossy().into_owned();
    let ts = name.strip_prefix("settings.json.ccbb-backup-").unwrap();
    assert_eq!(ts.len(), 15);
    assert!(chrono::NaiveDateTime::parse_from_str(ts, "%Y%m%d-%H%M%S").is_ok());
    assert_eq!(std::fs::metadata(&dest).unwrap().modified().unwrap(), old);
    assert_eq!(std::fs::read_to_string(dest).unwrap(), "{}\n");
}

// ---- status -----------------------------------------------------------------------------------------------------

#[derive(Default)]
struct FakeHost {
    listed: bool,
    calls: Mutex<Vec<Vec<String>>>,
}

impl Host for FakeHost {
    fn which(&self, name: &str) -> Option<PathBuf> {
        Some(PathBuf::from("/bin").join(name))
    }
    fn run(&self, argv: &[String]) -> io::Result<Completed> {
        self.calls.lock().unwrap().push(argv.to_vec());
        let out = if self.listed { "PID\tStatus\tLabel\n12\t0\tcom.github.eve-bridge.daemon\n" } else { "PID\tStatus\tLabel\n" };
        Ok(Completed::new(0, out, ""))
    }
    fn print(&self, _line: &str) {}
    fn eprint(&self, _line: &str) {}
    fn getuid(&self) -> io::Result<u32> {
        Ok(501)
    }
}

fn launchd_service(home: &Path, listed: bool) -> Service {
    let host: Arc<dyn Host> = Arc::new(FakeHost { listed, ..Default::default() });
    let launchd = Launchd {
        env: HomeEnv { home: Some(home.as_os_str().to_owned()), ..Default::default() },
        full: FullPaths::under(home),
        label: eve_service::service_launchd::LABEL.to_string(),
        launch: Launch::Native("/opt/eve/bin/eve-bridge".into()),
        host: host.clone(),
    };
    Service::with_backend("darwin", Some(Backend::Launchd(launchd)), host)
}

fn status(t: &Temp, service: Service) -> String {
    let mut c = Captured::default();
    assert_eq!(show_status_in(&t.env, None, || Ok(service), &mut c).unwrap(), 0);
    assert_eq!(c.err, "");
    c.out
}

#[test]
fn status_lists_our_hooks_and_an_unsupported_platform() {
    let t = temp_settings();
    write(
        &t.settings(),
        &json!({"hooks": {
            "Stop": [{"hooks": [{"command": "say hi"}, {"command": "/p -I -m eve_bridge.hooks.stop"}]}],
            "PreToolUse": [{"matcher": "Bash", "hooks": [{"command": "/p -I -m eve_bridge.hooks.pretooluse"}]},
                           {"matcher": null, "hooks": [{"command": "/p -I -m eve_bridge.hooks.pretooluse"}]}],
            "Odd": "not a list",
        }}),
    );
    let out = status(&t, Service::with_backend("plan9", None, Arc::new(FakeHost::default())));
    assert_eq!(
        out,
        format!(
            "Hooks: ({})\n  CLAUDE_CONFIG_DIR={}\n  Stop [*] → /p -I -m eve_bridge.hooks.stop\n  PreToolUse [Bash] → /p -I -m eve_bridge.hooks.pretooluse\n  PreToolUse [None] → /p -I -m eve_bridge.hooks.pretooluse\n\nService:\n  unsupported platform (plan9)\n",
            t.settings().display(),
            t.home.display()
        )
    );
}

#[test]
fn status_without_settings_or_hooks_or_service() {
    let t = temp_settings();
    let svc_home = TempDir::new().unwrap();
    let out = status(&t, launchd_service(svc_home.path(), false));
    assert_eq!(
        out,
        format!(
            "Hooks: ({0})\n  CLAUDE_CONFIG_DIR={1}\n  settings.json not found at {0}\n\nService (launchd):\n  not installed (run `eve-bridge install --service` to add)\n",
            t.settings().display(),
            t.home.display()
        )
    );

    write(&t.settings(), &json!({"hooks": null}));
    let out = status(&t, launchd_service(svc_home.path(), false));
    assert!(out.contains("\n  no eve-bridge hooks installed\n"));
}

#[test]
fn status_names_the_loaded_unit_and_its_log() {
    let t = temp_settings();
    let mut env = t.env.clone();
    env.claude_config_dir = None;
    let home = TempDir::new().unwrap();
    env.home = Some(home.path().as_os_str().to_owned());
    std::fs::create_dir(home.path().join(".claude")).unwrap();
    let svc = launchd_service(home.path(), true);
    let unit = FullPaths::under(home.path());
    std::fs::create_dir_all(unit.plist_path.parent().unwrap()).unwrap();
    std::fs::write(&unit.plist_path, "plist").unwrap();
    let mut c = Captured::default();
    show_status_in(&env, None, || Ok(svc), &mut c).unwrap();
    let settings = home.path().join(".claude").join("settings.json");
    assert_eq!(
        c.out,
        format!(
            "Hooks: ({})\n  settings.json not found at {}\n\nService (launchd):\n  loaded: {}\n  logs: {}\n",
            settings.display(),
            settings.display(),
            unit.plist_path.display(),
            unit.log_path.display()
        )
    );

    let svc = launchd_service(home.path(), false);
    let mut c = Captured::default();
    show_status_in(&env, None, || Ok(svc), &mut c).unwrap();
    assert!(c.out.contains("  installed but not loaded: "));
}

#[test]
fn status_prints_matchers_as_python_str() {
    let lines = installed_lines(&json!({"hooks": {"E": [
        {"matcher": 5, "hooks": [{"command": "eve_bridge.hooks.x"}]},
        {"matcher": ["a", "it's"], "hooks": [{"command": "eve_bridge.hooks.y"}]},
        {"matcher": {"k": true, "f": 1.0}, "hooks": [{"command": "eve_bridge.hooks.z"}]},
    ]}}))
    .unwrap();
    assert_eq!(
        lines,
        [
            "  E [5] → eve_bridge.hooks.x",
            "  E [['a', \"it's\"]] → eve_bridge.hooks.y",
            "  E [{'k': True, 'f': 1.0}] → eve_bridge.hooks.z",
        ]
    );
}
