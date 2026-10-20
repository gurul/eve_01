//! profile::install_firewall: eve lite's process firewall (port of test_profile_firewall.py).
//!
//! Every case that installs it runs in a fresh process (this test binary started again for that one case): the
//! firewall cannot be removed, so this process never installs one. The programs a case tries are fakes in a folder
//! of the test's own (each writes a mark if it ever runs), and the protected folders are under a fake HOME: nothing
//! real is started or opened, even if the firewall were broken. Real paths (/usr/bin/git, /usr/bin/osascript, ...)
//! are judged in this process by the guard's own check, without installing it. Each refusal case has its control:
//! the same case under full eve (no firewall) runs the fake, or opens the file.

mod common;

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use common::{is_child, print_result, run_child, write_program, Ran, CANARY};
use eve_profile::pypath;
use eve_profile::{self as profile, audit, audit_path, Arg, Event, FileEvent, Guard, Mode, Refused};
use serde_json::{json, Map, Value};

/// The lite LaunchAgent's PATH (spec M8).
const LITE_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";
const BLOCKED: &[&str] = &[
    "git", "clang", "xcrun", "osascript", "screencapture", "codex", "claude", "cloudflared", "adb", "python3", "make",
    "install_name_tool",
];

fn pid() -> u32 {
    std::process::id()
}

fn no_roots() -> Vec<(String, String)> {
    Vec::new()
}

const NO_CODE: &[&str] = &[];

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// A fake bin of programs that leave a mark when they run, a fake HOME with each kind of protected folder, and the
/// folder the marks go to.
struct Box_ {
    _dir: tempfile::TempDir,
    tmp: PathBuf,
    bin: PathBuf,
    marks: PathBuf,
    home: PathBuf,
}

fn make_box() -> Box_ {
    let dir = tempfile::tempdir().unwrap();
    let tmp = std::fs::canonicalize(dir.path()).unwrap();
    let (bin, marks, home) = (tmp.join("bin"), tmp.join("marks"), tmp.join("home"));
    std::fs::create_dir(&bin).unwrap();
    std::fs::create_dir(&marks).unwrap();
    for name in BLOCKED {
        write_program(&bin.join(name), &format!("#!/bin/sh\ntouch \"$MARKS/{name}\"\necho RAN-{name}\n"));
    }
    for sub in [
        "Documents", "Desktop", "Downloads", "Library/Messages", "Library/Mobile Documents/com~apple~CloudDocs",
        "Library/Containers/com.example.app", "Library/CloudStorage/Dropbox", ".config/eve-bridge/memory",
    ] {
        std::fs::create_dir_all(home.join(sub)).unwrap();
    }
    std::fs::write(home.join("Documents/a.txt"), "private").unwrap();
    std::fs::write(home.join(format!("Documents/{CANARY}.txt")), "private").unwrap();
    std::fs::write(home.join("Downloads/b.txt"), "private").unwrap();
    std::fs::write(home.join("Library/CloudStorage/Dropbox/d.txt"), "private").unwrap();
    std::fs::write(home.join("Library/Mobile Documents/com~apple~CloudDocs/i.txt"), "private").unwrap();
    std::fs::write(home.join(".config/eve-bridge/env"), "EVE_PROFILE=lite\n").unwrap();
    std::os::unix::fs::symlink(home.join("Documents"), home.join(".config/eve-bridge/docs")).unwrap();
    std::os::unix::fs::symlink(home.join("Library/CloudStorage/Dropbox"), home.join("Dropbox")).unwrap();
    Box_ { _dir: dir, tmp, bin, marks, home }
}

/// Act out the case `name` in a fresh process, with the firewall installed in `modes` (comma-separated, in order)
/// under EVE_PROFILE=lite, or under full eve when `lite` is false. `extra` adds to its environment.
fn run(b: &Box_, name: &str, lite: bool, modes: &str, path: Option<&str>, extra: &[(&str, String)]) -> Ran {
    let mut env = vec![
        ("PATH", path.map(str::to_string).unwrap_or_else(|| format!("{}:{LITE_PATH}", b.bin.display()))),
        ("HOME", s(&b.home)),
        ("MARKS", s(&b.marks)),
        ("FAKES", s(&b.bin)),
        ("FIREWALL_MODES", modes.to_string()),
        ("TMPDIR", s(&b.tmp)),
    ];
    if lite {
        env.push(("EVE_PROFILE", "lite".to_string()));
    }
    env.extend(extra.iter().cloned());
    run_child(name, &env, Some(&b.tmp))
}

// ---- what a case does in its own process ---------------------------------------------------------------------------

/// A case's results, by label, as the Python harness's `attempt` records them.
#[derive(Default)]
struct Attempts(Map<String, Value>);

impl Attempts {
    fn attempt(&mut self, label: &str, f: impl FnOnce() -> io::Result<Value>) {
        let result = match f() {
            Ok(value) => json!({"ran": true, "value": value}),
            Err(e) => match e.get_ref().and_then(|r| r.downcast_ref::<Refused>()) {
                Some(refused) => json!({"ran": false, "refused": refused.to_string()}),
                None => json!({"ran": false, "error": format!("{:?}", e.kind())}),
            },
        };
        self.0.insert(label.to_string(), result);
    }

    /// The case's result line: the attempts, firewall_counts, and the marks the fakes left.
    fn finish(self) {
        let marks = std::env::var("MARKS").unwrap();
        let mut left: Vec<String> =
            std::fs::read_dir(marks).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        print_result(&json!({"results": self.0, "counts": profile::firewall_counts(), "marks": left}));
    }
}

/// The start of every case: the log to stderr, then the firewall in each of FIREWALL_MODES.
fn begin() -> Attempts {
    common::log_to_stderr();
    for mode in std::env::var("FIREWALL_MODES").unwrap_or_default().split(',').filter(|m| !m.is_empty()) {
        profile::install_firewall(mode).unwrap();
    }
    Attempts::default()
}

fn stdout_of(cmd: &mut Command) -> io::Result<Value> {
    let done = profile::output(cmd)?;
    Ok(json!(String::from_utf8_lossy(&done.stdout).trim()))
}

fn code_of(cmd: &mut Command) -> io::Result<Value> {
    let done = profile::output(cmd)?;
    Ok(json!(done.status.code()))
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap())
}

// ---- programs -------------------------------------------------------------------------------------------------------

#[test]
fn test_developer_tools_and_the_macs_programs_are_refused() {
    let b = make_box();
    let lite = run(&b, "child_starts", true, "enforce", None, &[]);
    let results = &lite.result["results"];
    let labels: Vec<&str> = BLOCKED.iter().copied().chain(["shell", "os.system", "posix_spawnp", "posix_spawn"]).collect();
    for label in &labels {
        assert_eq!(results[label]["ran"], json!(false), "{label}: {}", results[label]);
        assert!(results[label]["refused"].is_string(), "{label}");
    }
    assert_eq!(lite.result["marks"], json!([])); // nothing ran, so nothing left a mark
    let counts = lite.result["counts"].as_object().unwrap();
    let mut want: Vec<String> = BLOCKED.iter().map(|n| format!("exec:{n}")).collect();
    want.push("exec:sh".into());
    want.sort();
    let mut got: Vec<String> = counts.keys().cloned().collect();
    got.sort();
    assert_eq!(got, want);
    assert_eq!(counts["exec:git"], json!(3));
    assert_eq!(counts["exec:sh"], json!(2));

    let full = run(&b, "child_starts", false, "enforce", None, &[]); // control: full eve has no firewall
    let results = &full.result["results"];
    for name in BLOCKED {
        assert_eq!(results[name], json!({"ran": true, "value": format!("RAN-{name}")}), "{name}");
    }
    assert_eq!(results["posix_spawnp"]["value"], json!("RAN-git"));
    assert_eq!(full.result["counts"], json!({}));
    let mut marks: Vec<&str> = full.result["marks"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    marks.sort();
    let mut blocked = BLOCKED.to_vec();
    blocked.sort();
    assert_eq!(marks, blocked);
}

#[test]
fn child_starts() {
    if !is_child("child_starts") {
        return;
    }
    let mut a = begin();
    let fakes = std::env::var("FAKES").unwrap();
    for name in BLOCKED {
        a.attempt(name, || stdout_of(&mut Command::new(name)));
    }
    a.attempt("shell", || stdout_of(Command::new("/bin/sh").args(["-c", "git"])));
    a.attempt("os.system", || {
        audit(&Event::System)?;
        Ok(json!(Command::new("/bin/sh").args(["-c", "git"]).status()?.code()))
    });
    a.attempt("posix_spawnp", || {
        audit(&Event::PosixSpawn { path: "git".into() })?;
        Ok(json!(String::from_utf8_lossy(&Command::new("git").output()?.stdout).trim()))
    });
    a.attempt("posix_spawn", || {
        let path = format!("{fakes}/git");
        audit(&Event::PosixSpawn { path: path.as_str().into() })?;
        Ok(json!(Command::new(path).status()?.code()))
    });
    a.finish();
}

#[test]
fn test_the_three_programs_it_allows_run() {
    // pmset, ifconfig and scutil run under the firewall, started the way lite starts them (quick_answers, watch,
    // lan_net) on the LaunchAgent's PATH, and through a link to them.
    for program in profile::EXEC_ALLOW {
        if !pypath::runnable(program) {
            eprintln!("skipped: {program} is not on this machine");
            return;
        }
    }
    let b = make_box();
    let links = b.tmp.join("links");
    std::fs::create_dir(&links).unwrap();
    std::os::unix::fs::symlink("/usr/bin/pmset", links.join("pmset")).unwrap(); // a link to pmset is pmset
    let out = run(&b, "child_allowed", true, "enforce", Some(LITE_PATH), &[("LINKS", s(&links))]);
    assert_eq!(
        out.result["results"],
        json!({"pmset": {"ran": true, "value": 0}, "ifconfig": {"ran": true, "value": 0},
               "scutil": {"ran": true, "value": true}, "linked pmset": {"ran": true, "value": 0}})
    );
    assert_eq!(out.result["counts"], json!({}));
}

#[test]
fn child_allowed() {
    if !is_child("child_allowed") {
        return;
    }
    let mut a = begin();
    a.attempt("pmset", || code_of(Command::new("pmset").args(["-g", "batt"])));
    a.attempt("ifconfig", || code_of(Command::new("/sbin/ifconfig").arg("-l")));
    a.attempt("scutil", || {
        let done = profile::output(Command::new("/usr/sbin/scutil").args(["--get", "LocalHostName"]))?;
        Ok(json!(matches!(done.status.code(), Some(0 | 1))))
    });
    let links = std::env::var("LINKS").unwrap();
    a.attempt("linked pmset", || code_of(Command::new(format!("{links}/pmset")).arg("-g")));
    a.finish();
}

#[test]
fn test_a_program_is_allowed_by_its_file_not_its_name() {
    // A fake named pmset ahead on the PATH is not pmset: refused (and a positive control for the case above).
    let b = make_box();
    write_program(&b.bin.join("pmset"), "#!/bin/sh\ntouch \"$MARKS/pmset\"\n");
    let out = run(&b, "child_fake_pmset", true, "enforce", None, &[]);
    assert_eq!(out.result["results"]["pmset"]["ran"], json!(false));
    assert_eq!(out.result["marks"], json!([]));
    assert_eq!(out.result["counts"], json!({"exec:pmset": 1}));
}

#[test]
fn child_fake_pmset() {
    if !is_child("child_fake_pmset") {
        return;
    }
    let mut a = begin();
    a.attempt("pmset", || code_of(&mut Command::new("pmset")));
    a.finish();
}

fn popen(path: &str, env: Option<HashMap<String, String>>) -> Event {
    Event::Popen { executable: path.into(), cwd: Arg::None, env }
}

#[test]
fn test_the_real_programs_are_judged_refused() {
    // Judged by the guard's own check, in this process and with nothing installed: nothing is started.
    let guard = Guard::new(Mode::Enforce, no_roots(), pid(), NO_CODE);
    for path in [
        "/usr/bin/git", "/usr/bin/clang", "/usr/bin/make", "/usr/bin/python3", "/usr/bin/install_name_tool",
        "/usr/bin/xcrun", "/usr/bin/osascript", "/usr/sbin/screencapture", "/bin/sh", "/usr/bin/open",
    ] {
        if !Path::new(path).exists() {
            continue;
        }
        let key = format!("exec:{}", pypath::basename(&pypath::realpath(path)));
        assert_eq!(guard.check(&popen(path, None)).unwrap().key, key, "{path}");
        assert_eq!(guard.check(&Event::PosixSpawn { path: path.into() }).unwrap().key, key, "{path}");
        assert_eq!(guard.check(&Event::Exec { path: path.into() }).unwrap().key, key, "{path}");
    }
}

#[test]
fn test_a_start_it_cannot_read_is_refused() {
    // Fails closed: an argument the guard cannot make a path of is a refusal, never a pass.
    let guard = Guard::new(Mode::Enforce, no_roots(), pid(), NO_CODE);
    let odd = Event::Popen { executable: Arg::Other("object".into()), cwd: Arg::None, env: None };
    assert_eq!(guard.check(&odd).unwrap().key, "exec:?");
    assert_eq!(guard.check(&Event::Exec { path: Arg::None }).unwrap().key, "exec:?");
    let odd_cwd = Event::Popen { executable: "git".into(), cwd: Arg::Fd(3), env: None };
    assert_eq!(guard.check(&odd_cwd).unwrap().key, "exec:?");
}

#[test]
fn test_an_allowed_program_that_became_a_link_is_still_allowed() {
    // A system path that a later macOS turns into a link (into a cryptex, say) is allowed by the file it reaches;
    // another program in that folder is not (the control).
    let dir = tempfile::tempdir().unwrap();
    let tmp = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::create_dir(tmp.join("cryptex")).unwrap();
    let real = tmp.join("cryptex/pmset");
    write_program(&real, "#!/bin/sh\n");
    let other = tmp.join("cryptex/other");
    write_program(&other, "#!/bin/sh\n");
    std::fs::create_dir(tmp.join("usr-bin")).unwrap();
    let link = tmp.join("usr-bin/pmset");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let guard = Guard::with_exec_allow(Mode::Enforce, no_roots(), pid(), NO_CODE, &[s(&link)]);
    assert_eq!(guard.check(&popen(&s(&link), None)), None);
    let path_env = HashMap::from([("PATH".to_string(), s(link.parent().unwrap()))]);
    assert_eq!(guard.check(&popen("pmset", Some(path_env))), None);
    assert_eq!(guard.check(&popen(&s(&other), None)).unwrap().key, "exec:other");
}

#[test]
fn test_the_three_real_programs_are_judged_allowed() {
    let guard = Guard::new(Mode::Enforce, no_roots(), pid(), NO_CODE);
    for path in profile::EXEC_ALLOW {
        if !Path::new(path).exists() {
            continue;
        }
        assert_eq!(guard.check(&popen(path, None)), None, "{path}");
        let env = HashMap::from([("PATH".to_string(), LITE_PATH.to_string())]);
        assert_eq!(guard.check(&popen(&pypath::basename(path), Some(env))), None, "{path}");
        assert_eq!(guard.check(&Event::System).unwrap().key, "exec:sh"); // a shell, whatever it would run
    }
}

#[test]
fn test_a_command_is_read_as_popen_reads_its_arguments() {
    // A Command's program, working folder and PATH are the event's executable, cwd and env.
    let mut cmd = Command::new("pmset");
    assert_eq!(profile::popen_event(&cmd), popen("pmset", None));
    cmd.current_dir("/tmp").env("PATH", "/usr/bin");
    assert_eq!(
        profile::popen_event(&cmd),
        Event::Popen {
            executable: "pmset".into(),
            cwd: "/tmp".into(),
            env: Some(HashMap::from([("PATH".to_string(), "/usr/bin".to_string())])),
        }
    );
    let mut cleared = Command::new("pmset");
    cleared.env_remove("PATH");
    assert_eq!(profile::popen_event(&cleared), popen("pmset", Some(HashMap::new())));
}

#[test]
fn test_a_forked_child_whose_exec_is_refused_exits() {
    let b = make_box();
    let lite = run(&b, "child_fork", true, "enforce", None, &[]);
    assert_eq!(lite.result["results"]["child"], json!({"ran": true, "value": 126}));
    assert_eq!(lite.result["marks"], json!([]));
    let full = run(&b, "child_fork", false, "enforce", None, &[]); // control: the exec runs
    assert_eq!(full.result["results"]["child"], json!({"ran": true, "value": 0}));
    assert_eq!(full.result["marks"], json!(["git"]));
}

#[test]
fn child_fork() {
    if !is_child("child_fork") {
        return;
    }
    let mut a = begin();
    let git = format!("{}/git", std::env::var("FAKES").unwrap());
    let path = std::ffi::CString::new(git.clone()).unwrap();
    let arg0 = std::ffi::CString::new("git").unwrap();
    let argv = [arg0.as_ptr(), std::ptr::null()];
    // SAFETY: the child only judges the exec and then execs or exits; the parent waits for it.
    let child = unsafe { libc::fork() };
    if child == 0 {
        let _ = audit(&Event::Exec { path: git.as_str().into() });
        // SAFETY: valid NUL-terminated strings and a NULL-terminated argv, built before the fork.
        unsafe {
            libc::execv(path.as_ptr(), argv.as_ptr());
            libc::_exit(99);
        }
    }
    a.attempt("child", || {
        let mut status = 0;
        // SAFETY: waiting on our own child.
        unsafe { libc::waitpid(child, &mut status, 0) };
        Ok(json!(if libc::WIFEXITED(status) { libc::WEXITSTATUS(status) } else { -1 }))
    });
    a.finish();
}

#[test]
fn test_a_fork_exec_start_is_a_process_start_too() {
    // Python's multiprocessing spawn starts the interpreter again through _posixsubprocess.fork_exec: the event is
    // judged like any start, by the first of its programs that runs (here this test binary, started again).
    let guard = Guard::new(Mode::Enforce, no_roots(), pid(), NO_CODE);
    let exe = s(&std::env::current_exe().unwrap());
    let event = Event::ForkExec { executables: vec![Arg::from("/nowhere/python"), exe.as_str().into()], cwd: Arg::None };
    let name = pypath::basename(&pypath::realpath(&exe));
    assert_eq!(guard.check(&event).unwrap().key, format!("exec:{name}"));
    assert_eq!(guard.check(&Event::ForkExec { executables: vec!["/nowhere/python".into()], cwd: Arg::None }), None);
}

// ---- files ----------------------------------------------------------------------------------------------------------

const REFUSED: &[&str] = &[
    "open", "open, another case", "open, through a link", "open, through ..", "open, data volume", "listdir",
    "scandir", "iCloud Drive", "other apps' data", "a cloud drive, through ~/Dropbox", "sqlite", "sqlite uri", "mkdir",
    "rename into", "volumes", "relative", "listdir here",
];
const ALLOWED: &[&str] =
    &["eve's env file", "eve's memory", "the home itself", "its Library", "a temp file", "memory db"];

fn listing(dir: &str) -> io::Result<Value> {
    let mut names: Vec<String> =
        std::fs::read_dir(dir)?.map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned())).collect::<Result<_, _>>()?;
    names.sort();
    Ok(json!(names))
}

fn read(path: &str) -> io::Result<Value> {
    audit_path(FileEvent::Open, [path])?;
    Ok(json!(std::fs::read_to_string(path)?))
}

#[test]
fn test_protected_folders_are_refused_and_eves_own_are_not() {
    let b = make_box();
    let lite = run(&b, "child_files", true, "enforce", None, &[]);
    let results = &lite.result["results"];
    for label in REFUSED {
        assert_eq!(results[label]["ran"], json!(false), "{label}: {}", results[label]);
        assert!(results[label]["refused"].is_string(), "{label}: {}", results[label]);
    }
    for label in ALLOWED {
        assert_eq!(results[label]["ran"], json!(true), "{label}: {}", results[label]);
    }
    assert_eq!(results["eve's env file"]["value"], json!("EVE_PROFILE=lite"));
    let mut got: Vec<String> = lite.result["counts"].as_object().unwrap().keys().cloned().collect();
    got.sort();
    let mut want: Vec<String> = [
        "path:~/Documents", "path:~/Desktop", "path:~/Downloads", "path:~/Library/Messages",
        "path:~/Library/Mobile Documents", "path:~/Library/Containers", "path:~/Library/CloudStorage", "path:/Volumes",
    ]
    .iter()
    .map(|k| k.to_string())
    .collect();
    want.sort();
    assert_eq!(got, want);
    assert!(b.home.join(".config/eve-bridge/env").exists() && !b.home.join("Documents/new").exists());

    let full = run(&b, "child_files", false, "enforce", None, &[]); // control: full eve opens every one of them
    for label in REFUSED.iter().filter(|l| **l != "volumes") {
        assert_eq!(full.result["results"][label]["ran"], json!(true), "{label}: {}", full.result["results"][label]);
    }
    assert_eq!(full.result["results"]["open, through a link"]["value"], json!("private"));
    assert_eq!(full.result["counts"], json!({}));
}

#[test]
fn child_files() {
    if !is_child("child_files") {
        return;
    }
    let mut a = begin();
    let home = s(&home());
    let d = format!("{home}/Documents");
    a.attempt("open", || read(&format!("{d}/a.txt")));
    a.attempt("open, another case", || read(&format!("{home}/documents/a.txt")));
    a.attempt("open, through a link", || read(&format!("{home}/.config/eve-bridge/docs/a.txt")));
    a.attempt("open, through ..", || read(&format!("{home}/.config/eve-bridge/../../Documents/a.txt")));
    a.attempt("open, data volume", || read(&format!("/System/Volumes/Data{}/a.txt", pypath::realpath(&d))));
    let list = |event: FileEvent, dir: String| -> io::Result<Value> {
        audit_path(event, [dir.as_str()])?;
        listing(&dir)
    };
    a.attempt("listdir", || list(FileEvent::Listdir, format!("{home}/Desktop")));
    a.attempt("scandir", || list(FileEvent::Scandir, format!("{home}/Downloads")));
    a.attempt("iCloud Drive", || list(FileEvent::Listdir, format!("{home}/Library/Mobile Documents/com~apple~CloudDocs")));
    a.attempt("other apps' data", || list(FileEvent::Listdir, format!("{home}/Library/Containers")));
    a.attempt("a cloud drive, through ~/Dropbox", || read(&format!("{home}/Dropbox/d.txt")));
    let chat = format!("{home}/Library/Messages/chat.db");
    a.attempt("sqlite", || {
        audit_path(FileEvent::SqliteConnect, [chat.as_str()])?;
        std::fs::OpenOptions::new().create(true).append(true).open(&chat)?; // what connecting creates
        Ok(Value::Null)
    });
    a.attempt("sqlite uri", || {
        audit_path(FileEvent::SqliteConnect, [format!("file:{chat}?mode=ro")])?;
        std::fs::File::open(&chat)?;
        Ok(Value::Null)
    });
    a.attempt("mkdir", || {
        let new = format!("{d}/new");
        audit_path(FileEvent::Mkdir, [new.as_str()])?;
        std::fs::create_dir(&new)?;
        Ok(Value::Null)
    });
    a.attempt("rename into", || {
        let (from, to) = (format!("{home}/.config/eve-bridge/env"), format!("{d}/env"));
        audit_path(FileEvent::Rename, [from.as_str(), to.as_str()])?;
        std::fs::rename(&from, &to)?;
        Ok(Value::Null)
    });
    if std::env::var("EVE_PROFILE").as_deref() == Ok("lite") {
        // the control never lists the real /Volumes
        a.attempt("volumes", || list(FileEvent::Listdir, "/Volumes".to_string()));
    }
    std::env::set_current_dir(&home).unwrap();
    a.attempt("relative", || read("Documents/a.txt"));
    std::env::set_current_dir(format!("{home}/Desktop")).unwrap();
    a.attempt("listdir here", || {
        audit(&Event::file(FileEvent::Listdir, [Arg::None]))?;
        listing(".")
    });
    std::env::set_current_dir(&home).unwrap();
    a.attempt("eve's env file", || {
        Ok(json!(read(&format!("{home}/.config/eve-bridge/env"))?.as_str().unwrap_or("").trim()))
    });
    a.attempt("eve's memory", || {
        let path = format!("{home}/.config/eve-bridge/memory/x.md");
        audit_path(FileEvent::Open, [path.as_str()])?;
        std::fs::write(&path, "ok")?;
        Ok(json!(2))
    });
    a.attempt("the home itself", || Ok(json!(list(FileEvent::Listdir, home.clone())?.as_array().map_or(0, Vec::len) > 0)));
    a.attempt("its Library", || {
        let names = list(FileEvent::Listdir, format!("{home}/Library"))?;
        Ok(json!(names.as_array().unwrap().contains(&json!("Messages"))))
    });
    a.attempt("a temp file", || {
        let path = format!("{}/tmp-{}", std::env::var("TMPDIR").unwrap(), std::process::id());
        audit_path(FileEvent::Open, [path.as_str()])?;
        std::fs::write(&path, "")?;
        Ok(json!(true))
    });
    a.attempt("memory db", || {
        audit_path(FileEvent::SqliteConnect, [":memory:"])?;
        Ok(Value::Null)
    });
    a.finish();
}

#[test]
fn test_a_path_is_judged_by_its_words_first() {
    // Inside the folder (in any case) is refused before anything is looked at; beside it, or not a path, is not.
    let guard = Guard::new(Mode::Enforce, [("/nowhere/home/Documents".to_string(), "~/Documents".to_string())], pid(), NO_CODE);
    assert_eq!(guard.reaches(&"/nowhere/home/Documents/a/b/c.txt".into()).as_deref(), Some("~/Documents"));
    assert_eq!(guard.reaches(&"/NOWHERE/home/documents".into()).as_deref(), Some("~/Documents"));
    assert_eq!(guard.reaches(&"/nowhere/home/Documents2/x".into()), None);
    assert_eq!(guard.reaches(&"/nowhere/home".into()), None);
    assert_eq!(guard.reaches(&Arg::Fd(3)), None); // a descriptor
    assert_eq!(guard.reaches(&Arg::None), None); // the working folder
    assert_eq!(guard.reaches(&"".into()), None);
    assert_eq!(guard.reaches(&"/nowhere/home/Documents/\0".into()), None);
    assert_eq!(guard.reaches(&"//nowhere/home/Documents/x".into()).as_deref(), Some("~/Documents"));
}

#[test]
fn test_following_a_link_stops_at_the_protected_folder() {
    // Every lstat the walk makes is recorded: the links outside are followed, nothing at or under ~/Documents is
    // looked at, and the path is still refused. The control: a link to a folder outside is followed to its end.
    let dir = tempfile::tempdir().unwrap();
    let home = std::fs::canonicalize(dir.path()).unwrap().join("home");
    std::fs::create_dir_all(home.join("Documents/deep")).unwrap();
    std::fs::create_dir(home.join("Public")).unwrap();
    std::os::unix::fs::symlink(home.join("b"), home.join("a")).unwrap();
    std::os::unix::fs::symlink(home.join("Documents/deep"), home.join("b")).unwrap(); // a chain into the folder
    std::os::unix::fs::symlink(home.join("Public"), home.join("c")).unwrap();
    let looked: Arc<Mutex<Vec<String>>> = Arc::default();
    let record = looked.clone();
    let env = HashMap::from([("HOME".to_string(), s(&home))]);
    let guard = Guard::new(Mode::Enforce, profile::protected_roots(Some(&env)), pid(), NO_CODE).with_link_check(
        move |path| {
            record.lock().unwrap().push(path.to_string());
            pypath::is_link(path)
        },
    );
    let inside = s(&home.join("Documents"));
    assert_eq!(guard.reaches(&s(&home.join("a/x.txt")).into()).as_deref(), Some("~/Documents"));
    let walked = looked.lock().unwrap().clone();
    assert!(walked.contains(&s(&home.join("a"))) && walked.contains(&s(&home.join("b"))), "{walked:?}");
    assert!(!walked.iter().any(|p| *p == inside || p.starts_with(&format!("{inside}/"))), "{walked:?}");
    looked.lock().unwrap().clear();
    assert_eq!(guard.reaches(&s(&home.join("c/y.txt")).into()), None);
    assert!(looked.lock().unwrap().contains(&s(&home.join("Public/y.txt"))));
}

#[test]
fn test_a_loop_of_links_reaches_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let tmp = std::fs::canonicalize(dir.path()).unwrap();
    std::os::unix::fs::symlink(tmp.join("y"), tmp.join("x")).unwrap();
    std::os::unix::fs::symlink(tmp.join("x"), tmp.join("y")).unwrap();
    let env = HashMap::from([("HOME".to_string(), s(&tmp))]);
    let guard = Guard::new(Mode::Enforce, profile::protected_roots(Some(&env)), pid(), NO_CODE);
    assert_eq!(guard.reaches(&s(&tmp.join("x/f")).into()), None);
}

#[test]
fn test_this_processs_own_code_stays_readable_inside_a_protected_folder() {
    // A developer's checkout under ~/Documents: the code folders are exempt, the rest of the folder is not.
    let dir = tempfile::tempdir().unwrap();
    let tmp = std::fs::canonicalize(dir.path()).unwrap();
    let documents = tmp.join("Documents");
    let code = documents.join("checkout/src");
    let roots = [(s(&documents), "~/Documents".to_string())];
    let guard = Guard::new(Mode::Enforce, roots.clone(), pid(), &[s(&code)]);
    assert_eq!(guard.reaches(&s(&code.join("eve_bridge/telegram.py")).into()), None);
    assert_eq!(guard.reaches(&s(&code).into()), None);
    assert_eq!(guard.reaches(&s(&documents.join("checkout/notes.txt")).into()).as_deref(), Some("~/Documents"));
    assert_eq!(guard.reaches(&s(&documents.join("checkout/src2/x.py")).into()).as_deref(), Some("~/Documents"));
    // a code folder outside the protected ones with a link into one: exempt by its words only, so still refused
    let venv = tmp.join("venv");
    std::fs::create_dir(&venv).unwrap();
    std::os::unix::fs::symlink(&documents, venv.join("docs")).unwrap();
    let guard = Guard::new(Mode::Enforce, roots, pid(), &[s(&venv)]);
    assert_eq!(guard.reaches(&s(&venv.join("docs/a.txt")).into()).as_deref(), Some("~/Documents"));
    assert_eq!(guard.reaches(&s(&venv.join("lib/x.py")).into()), None);
    let found = profile::code_roots();
    let exe_dir = pypath::dirname(&s(&std::env::current_exe().unwrap()));
    assert!(found.contains(&pypath::normpath(&exe_dir)) || found.contains(&pypath::realpath(&exe_dir)), "{found:?}");
    let src = pypath::normpath(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/src"));
    assert!(found.contains(&src) || found.contains(&pypath::realpath(&src)), "{found:?}");
    assert!(!found.contains(&"/".to_string()));
}

#[test]
fn test_a_link_inside_an_exempt_checkout_into_a_protected_folder_is_still_refused() {
    // Review A6 (2026-10-18): a code folder inside ~/Documents was exempt by its words alone, so a link in it to
    // ~/Desktop (or to the rest of ~/Documents) was read straight through. Its links are followed now.
    let dir = tempfile::tempdir().unwrap();
    let tmp = std::fs::canonicalize(dir.path()).unwrap();
    let (documents, desktop) = (tmp.join("Documents"), tmp.join("Desktop"));
    let code = documents.join("checkout/src");
    std::fs::create_dir_all(code.join("pkg")).unwrap();
    std::fs::create_dir(&desktop).unwrap();
    std::os::unix::fs::symlink(&desktop, code.join("desk")).unwrap();
    std::os::unix::fs::symlink(documents.join("checkout"), code.join("notes")).unwrap();
    std::os::unix::fs::symlink(code.join("pkg"), code.join("same")).unwrap(); // control: stays inside the code
    let roots = [(s(&documents), "~/Documents".to_string()), (s(&desktop), "~/Desktop".to_string())];
    let guard = Guard::new(Mode::Enforce, roots, pid(), &[s(&code)]);
    assert_eq!(guard.reaches(&s(&code.join("desk/secret.txt")).into()).as_deref(), Some("~/Desktop"));
    assert_eq!(guard.reaches(&s(&code.join("notes/todo.txt")).into()).as_deref(), Some("~/Documents"));
    assert_eq!(guard.reaches(&s(&code.join("same/x.py")).into()), None);
    assert_eq!(guard.reaches(&s(&code.join("pkg/x.py")).into()), None);
}

#[test]
fn test_protected_roots_are_each_folder_as_given_resolved_and_on_the_data_volume() {
    let env = HashMap::from([("HOME".to_string(), "/nowhere/someone/".to_string())]);
    let roots = profile::protected_roots(Some(&env));
    assert_eq!(roots.len(), profile::PROTECTED_HOME.len() * 2 + 1);
    assert_eq!(roots[0], ("/nowhere/someone/Desktop".to_string(), "~/Desktop".to_string()));
    assert_eq!(roots[1], ("/System/Volumes/Data/nowhere/someone/Desktop".to_string(), "~/Desktop".to_string()));
    assert_eq!(roots.last().unwrap(), &("/Volumes".to_string(), "/Volumes".to_string()));
    // HOME unset or relative: the account's own
    let relative = HashMap::from([("HOME".to_string(), "someone".to_string())]);
    assert!(profile::protected_roots(Some(&relative)).iter().all(|(p, _)| p.starts_with('/')));
}

// ---- modes, logs, counts --------------------------------------------------------------------------------------------

#[test]
fn test_audit_mode_lets_everything_through_and_counts_it() {
    let b = make_box();
    let out = run(&b, "child_audit", true, "audit", None, &[]);
    assert_eq!(
        out.result["results"],
        json!({"git 0": {"ran": true, "value": "RAN-git"}, "git 1": {"ran": true, "value": "RAN-git"},
               "open": {"ran": true, "value": "private"}})
    );
    assert_eq!(out.result["counts"], json!({"exec:git": 2, "path:~/Documents": 1}));
    assert_eq!(out.result["marks"], json!(["git"]));
    assert_eq!(out.log.matches("would refuse the program git").count(), 1, "{}", out.log); // said once, counted twice
}

#[test]
fn child_audit() {
    if !is_child("child_audit") {
        return;
    }
    let mut a = begin();
    for i in 0..2 {
        a.attempt(&format!("git {i}"), || stdout_of(&mut Command::new("git")));
    }
    a.attempt("open", || read(&format!("{}/Documents/a.txt", s(&home()))));
    a.finish();
}

#[test]
fn test_enforcing_cannot_be_loosened() {
    let b = make_box();
    for modes in ["audit,enforce", "enforce,audit", "enforce,enforce"] {
        let out = run(&b, "child_git_once", true, modes, None, &[]);
        assert_eq!(out.result["results"]["git"]["ran"], json!(false), "{modes}");
        assert_eq!(out.result["marks"], json!([]), "{modes}");
    }
}

#[test]
fn child_git_once() {
    if !is_child("child_git_once") {
        return;
    }
    let mut a = begin();
    a.attempt("git", || code_of(&mut Command::new("git")));
    a.finish();
}

#[test]
fn test_the_log_names_programs_and_folders_never_values() {
    let b = make_box();
    let out = run(&b, "child_canary", true, "enforce", None, &[]);
    assert_eq!(out.result["results"]["git"]["ran"], json!(false));
    assert_eq!(out.result["results"]["open"]["ran"], json!(false));
    let lines: Vec<&str> = out.log.lines().filter(|l| l.starts_with("eve_profile")).collect();
    assert!(lines.iter().any(|l| l.contains("the program git")), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("~/Documents")), "{lines:?}");
    let home = s(&b.home);
    assert!(!out.log.contains(CANARY) && !out.log.contains(&home));
    let results = out.result["results"].to_string();
    assert!(!results.contains(CANARY) && !results.contains(&home));
}

#[test]
fn child_canary() {
    if !is_child("child_canary") {
        return;
    }
    let mut a = begin();
    a.attempt("git", || code_of(Command::new("git").args(["--token", CANARY])));
    a.attempt("open", || read(&format!("{}/Documents/{CANARY}.txt", s(&home()))));
    a.finish();
}

#[test]
fn test_counts_are_a_copy() {
    assert!(profile::firewall_counts().is_empty()); // no firewall in this process
    let guard = Guard::new(Mode::Audit, no_roots(), pid(), NO_CODE); // never installed here
    guard.note("exec:git", "the program git");
    let mut counts = guard.counts();
    counts.insert("exec:git".into(), 99);
    assert_eq!(guard.counts().into_iter().collect::<Vec<_>>(), vec![("exec:git".to_string(), 1)]);
}

#[test]
fn test_an_audit_guard_lets_an_event_through_and_an_enforcing_one_refuses_it() {
    // Without installing: the guard's own call, as the hook is called.
    let roots = [("/nowhere/home/Documents".to_string(), "~/Documents".to_string())];
    let event = Event::file(FileEvent::Open, ["/nowhere/home/Documents/a.txt"]);
    let auditing = Guard::new(Mode::Audit, roots.clone(), pid(), NO_CODE);
    assert!(auditing.call(&event).is_ok());
    let enforcing = Guard::new(Mode::Enforce, roots, pid(), NO_CODE);
    let err = enforcing.call(&event).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(err.to_string(), "[Errno 1] eve lite stays out of ~/Documents");
    assert_eq!(err.get_ref().unwrap().downcast_ref::<Refused>().unwrap().errno(), libc::EPERM);
    assert_eq!(enforcing.counts()["path:~/Documents"], 1);
    assert!(enforcing.call(&Event::file(FileEvent::Symlink, ["/nowhere/home/Documents/x", "/elsewhere"])).is_ok());
    assert!(enforcing.call(&Event::file(FileEvent::Symlink, ["/elsewhere", "/nowhere/home/Documents/x"])).is_err());
}
