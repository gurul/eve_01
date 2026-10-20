//! Port of test_toolchain.py: whether the developer tools are on this Mac, and whether "git" runs git, found without
//! starting a process.
//!
//! Every fake toolchain here is a folder of the test's own: the locations (clt_dir, xcode_glob, select_link,
//! shim_dir) are pointed at it, so no test depends on what this Mac has installed, and no test runs a real shim.

use std::collections::HashMap;
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use super::*;

fn program_at(path: &Path) -> PathBuf {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.to_path_buf()
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// A Mac with no developer tools anywhere xcode-select looks: every location is an empty folder of the test's. A Mac
/// wherever the test runs: git_usable reads the shim rules on macOS only.
struct NoTools {
    _tmp: tempfile::TempDir,
    tmp: PathBuf,
    root: PathBuf,
    tc: Toolchain,
}

fn no_tools() -> NoTools {
    let tmp = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let root = base.join("mac");
    let tc = Toolchain {
        shim_dir: SHIM_DIR.to_string(),
        select_link: s(&root.join("db").join("xcode_select_link")),
        xcode_glob: s(&root.join("Applications").join("Xcode*.app").join("Contents").join("Developer")),
        clt_dir: s(&root.join("CommandLineTools")),
        platform: "darwin".to_string(),
    };
    NoTools { _tmp: tmp, tmp: base, root, tc }
}

fn git_in(developer_dir: &Path) -> PathBuf {
    program_at(&developer_dir.join("usr").join("bin").join("git"))
}

// ---- dev_tools_present: the places xcode-select looks ----------------------------------------------------------

#[test]
fn test_no_developer_directory_anywhere_is_no_tools() {
    let t = no_tools();
    assert!(!t.tc.dev_tools_present(&env(&[])));
    assert_eq!(t.tc.developer_dirs(&env(&[])), vec![t.tc.clt_dir.clone()]);
}

#[test]
fn test_the_command_line_tools_count() {
    let t = no_tools();
    git_in(Path::new(&t.tc.clt_dir));
    assert!(t.tc.dev_tools_present(&env(&[])));
}

#[test]
fn test_a_git_that_cannot_run_is_not_the_tools() {
    let t = no_tools();
    let git = git_in(Path::new(&t.tc.clt_dir));
    std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(!t.tc.dev_tools_present(&env(&[])));
}

#[test]
fn test_any_xcode_in_applications_counts() {
    let t = no_tools();
    git_in(&t.root.join("Applications").join("Xcode-beta.app").join("Contents").join("Developer"));
    assert!(t.tc.dev_tools_present(&env(&[])));
    assert!(t.tc.developer_dirs(&env(&[]))[0].ends_with("Xcode-beta.app/Contents/Developer"));
}

#[test]
fn test_xcodes_in_applications_come_sorted() {
    let t = no_tools();
    for name in ["Xcode.app", "Xcode-beta.app", "Xcode_15.app"] {
        git_in(&t.root.join("Applications").join(name).join("Contents").join("Developer"));
    }
    let dirs = t.tc.developer_dirs(&env(&[]));
    let names: Vec<&str> = dirs[..3].iter().map(|d| d.split('/').rev().nth(2).unwrap()).collect();
    assert_eq!(names, vec!["Xcode-beta.app", "Xcode.app", "Xcode_15.app"]);
    assert_eq!(dirs[3], t.tc.clt_dir);
}

#[test]
fn test_the_folder_xcode_select_chose_counts() {
    let t = no_tools();
    let chosen =
        t.root.join("Users").join("someone").join("Applications").join("Xcode.app").join("Contents").join("Developer");
    let link = PathBuf::from(&t.tc.select_link);
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    symlink(&chosen, &link).unwrap();
    assert!(!t.tc.dev_tools_present(&env(&[]))); // chosen, but nothing there (control)
    git_in(&chosen);
    assert!(t.tc.dev_tools_present(&env(&[])));
}

#[test]
fn test_a_relative_select_link_reads_from_its_folder() {
    let t = no_tools();
    let link = PathBuf::from(&t.tc.select_link);
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    symlink("../Xcode.app/Contents/Developer", &link).unwrap();
    let dirs = t.tc.developer_dirs(&env(&[]));
    assert_eq!(dirs[0], format!("{}/db/../Xcode.app/Contents/Developer", s(&t.root)));
    git_in(&t.root.join("Xcode.app").join("Contents").join("Developer"));
    assert!(t.tc.dev_tools_present(&env(&[])));
}

#[test]
fn test_developer_dir_counts_as_xcrun_reads_it() {
    let t = no_tools();
    let xcode = t.root.join("Elsewhere").join("Xcode.app");
    git_in(&xcode.join("Contents").join("Developer"));
    assert!(t.tc.dev_tools_present(&env(&[(DEVELOPER_DIR_ENV, &s(&xcode))]))); // the .app
    assert!(t.tc.dev_tools_present(&env(&[(DEVELOPER_DIR_ENV, &s(&xcode.join("Contents").join("Developer")))]))); // its Developer
    assert!(!t.tc.dev_tools_present(&env(&[(DEVELOPER_DIR_ENV, &s(&t.root.join("nothing")))])));
    // surrounding whitespace is stripped, and a blank value is no choice at all
    let padded = format!("  {}\n", s(&xcode));
    assert!(t.tc.dev_tools_present(&env(&[(DEVELOPER_DIR_ENV, &padded)])));
    assert_eq!(t.tc.developer_dirs(&env(&[(DEVELOPER_DIR_ENV, "  ")])), vec![t.tc.clt_dir.clone()]);
}

/// Words that name a way to start a process from Rust.
fn starts_processes(source: &str) -> Vec<&'static str> {
    ["Command::", "process::", "spawn(", "fork(", "execv", "posix_spawn", "libc::system"]
        .into_iter()
        .filter(|w| source.contains(w))
        .collect()
}

#[test]
fn test_dev_tools_present_and_git_usable_start_no_process() {
    // Python runs the functions under an audit hook listing every process start. Rust has no such hook, so the
    // proof is the source: nothing in this crate's code (or eve-profile's pypath, which it reads paths with) names a
    // way to start a process. The same scan finds one in a line that does (positive control).
    for (file, source) in [
        ("lib.rs", include_str!("lib.rs")),
        ("pyglob.rs", include_str!("pyglob.rs")),
        ("pypath.rs", include_str!("../../eve-profile/src/pypath.rs")),
    ] {
        // the code only: comments explain what the shims do and may name these words
        let code: String = source
            .split("#[cfg(test)]")
            .next()
            .unwrap()
            .lines()
            .map(|line| line.split("//").next().unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(starts_processes(&code), Vec::<&str>::new(), "{file}");
    }
    assert_eq!(starts_processes("std::process::Command::new(\"/usr/bin/true\").status()"), vec!["Command::", "process::"]);
    // and the answers come back without one
    let t = no_tools();
    let _ = (t.tc.dev_tools_present(&ProcessEnv), t.tc.git_usable(&ProcessEnv), t.tc.developer_dirs(&ProcessEnv));
}

// ---- which and is_shim: what a subprocess would run ------------------------------------------------------------

#[test]
fn test_which_follows_subprocess_rules() {
    let t = no_tools();
    let (a, b) = (t.tmp.join("a"), t.tmp.join("b"));
    program_at(&b.join("tool"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::write(a.join("tool"), "not a program").unwrap(); // not executable: skipped, as exec skips it
    assert_eq!(which("tool", Some(&env(&[("PATH", &format!("{}:{}", s(&a), s(&b)))]))), Some(s(&b.join("tool"))));
    assert_eq!(which("tool", Some(&env(&[("PATH", &s(&a))]))), None);
    // the empty entry (the working folder) is tested in tests/which_cwd.rs, alone in its process: it changes the cwd
    // no PATH at all: os.defpath, where subprocess looks too (os.get_exec_path)
    let expected = pypath::DEFPATH
        .split(':')
        .map(|d| format!("{d}/true"))
        .find(|p| program(p));
    assert_eq!(which("true", Some(&env(&[]))), expected);
    // a folder is not a program
    std::fs::create_dir_all(t.tmp.join("c").join("tool")).unwrap();
    assert_eq!(which("tool", Some(&env(&[("PATH", &s(&t.tmp.join("c")))]))), None);
}

#[test]
fn test_is_shim_follows_links_and_ignores_case() {
    let t = no_tools();
    let shim_dir = t.tmp.join("usr-bin");
    let shim = program_at(&shim_dir.join("git"));
    let tc = Toolchain { shim_dir: s(&shim_dir), ..t.tc.clone() };
    let link = t.tmp.join("links").join("git");
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    symlink(&shim, &link).unwrap();
    assert!(tc.is_shim(&s(&shim)) && tc.is_shim(&s(&link)));
    assert!(!tc.is_shim(&s(&program_at(&t.tmp.join("homebrew").join("git")))));
    let upper = PathBuf::from(s(&shim_dir).to_uppercase()).join("git");
    if upper.exists() {
        // a case-insensitive volume (APFS's default)
        assert!(tc.is_shim(&s(&upper)));
    }
}

// ---- git_usable ------------------------------------------------------------------------------------------------

#[test]
fn test_git_usable_is_never_true_in_eve_lite() {
    let t = no_tools();
    let brew = program_at(&t.tmp.join("brew").join("bin").join("git")).parent().unwrap().to_path_buf();
    git_in(Path::new(&t.tc.clt_dir));
    assert!(t.tc.git_usable(&env(&[("PATH", &s(&brew))])).unwrap()); // control: full eve runs it
    assert!(!t.tc.git_usable(&env(&[("PATH", &s(&brew)), ("EVE_PROFILE", "lite")])).unwrap());
    assert!(t.tc.git_usable(&env(&[("PATH", &s(&brew)), ("EVE_PROFILE", "full")])).unwrap());
}

#[test]
fn test_a_bad_profile_is_an_error_as_in_python() {
    let t = no_tools();
    let err = t.tc.git_usable(&env(&[("EVE_PROFILE", "medium")])).unwrap_err();
    assert!(err.to_string().starts_with("EVE_PROFILE must be"), "{err}");
}

#[test]
fn test_a_git_outside_usr_bin_is_git_without_the_tools() {
    let t = no_tools();
    let brew = program_at(&t.tmp.join("brew").join("bin").join("git")).parent().unwrap().to_path_buf();
    assert!(!t.tc.dev_tools_present(&env(&[])));
    assert!(t.tc.git_usable(&env(&[("PATH", &s(&brew))])).unwrap());
}

#[test]
fn test_usr_bin_git_counts_only_with_the_tools() {
    let t = no_tools();
    let shim_dir = t.tmp.join("usr-bin");
    let shim = program_at(&shim_dir.join("git"));
    let tc = Toolchain { shim_dir: s(&shim_dir), ..t.tc.clone() };
    let links = t.tmp.join("links");
    std::fs::create_dir_all(&links).unwrap();
    symlink(&shim, links.join("git")).unwrap();
    for path in [s(&shim_dir), s(&links)] {
        // the shim, no tools: running it is the dialog
        assert!(!tc.git_usable(&env(&[("PATH", &path)])).unwrap(), "{path}");
    }
    git_in(Path::new(&tc.clt_dir));
    for path in [s(&shim_dir), s(&links)] {
        assert!(tc.git_usable(&env(&[("PATH", &path)])).unwrap(), "{path}");
    }
}

#[test]
fn test_no_git_at_all_is_not_usable() {
    let t = no_tools();
    git_in(Path::new(&t.tc.clt_dir));
    std::fs::create_dir_all(t.tmp.join("empty")).unwrap();
    assert!(!t.tc.git_usable(&env(&[("PATH", &s(&t.tmp.join("empty")))])).unwrap());
}

#[test]
fn test_off_macos_git_runs_as_it_always_has() {
    // Linux keeps real git in /usr/bin and has none of the folders xcode-select looks in, and on Windows the file is
    // git.exe: neither has a shim, so git_usable is true there and full eve keeps its records' history (bridge/README
    // lists macOS, Linux and Windows). Control: the same folder on a Mac without the tools is the shim.
    let t = no_tools();
    let usr_bin = t.tmp.join("usr").join("bin");
    program_at(&usr_bin.join("git"));
    let mut tc = Toolchain { shim_dir: s(&usr_bin), ..t.tc.clone() };
    let e = env(&[("PATH", &s(&usr_bin))]);
    assert!(tc.is_shim(&s(&usr_bin.join("git"))) && !tc.dev_tools_present(&e));
    for platform in ["linux", "win32"] {
        tc.platform = platform.to_string();
        assert!(tc.git_usable(&e).unwrap(), "{platform}");
        assert!(tc.git_usable(&env(&[])).unwrap(), "{platform}"); // no git found: spawning says so, as it always did
        let mut lite = e.clone();
        lite.insert("EVE_PROFILE".into(), "lite".into());
        assert!(!tc.git_usable(&lite).unwrap(), "{platform}");
    }
    tc.platform = "darwin".to_string();
    assert!(!tc.git_usable(&e).unwrap()); // control: a Mac without the tools
}

#[test]
fn test_sys_platform_names_this_build_as_python_does() {
    let want = if cfg!(target_os = "macos") { "darwin" } else if cfg!(target_os = "linux") { "linux" } else { "" };
    if !want.is_empty() {
        assert_eq!(sys_platform(), want);
        assert_eq!(Toolchain::system().platform, want);
    }
    let sys = Toolchain::default();
    assert_eq!(
        (sys.shim_dir.as_str(), sys.select_link.as_str(), sys.xcode_glob.as_str(), sys.clt_dir.as_str()),
        (SHIM_DIR, SELECT_LINK, XCODE_GLOB, CLT_DIR)
    );
}

#[test]
fn test_this_mac() {
    // On a Mac that has the Command Line Tools (the owner's), they are found, and its git is usable in full eve:
    // nothing changes for the owner.
    if !program("/Library/Developer/CommandLineTools/usr/bin/git") {
        eprintln!("skipped: no Command Line Tools on this machine");
        return;
    }
    let mut e: HashMap<String, String> = std::env::vars().collect();
    e.remove("EVE_PROFILE");
    assert!(dev_tools_present(Some(&e)));
    if which("git", Some(&e)).is_some() {
        assert!(git_usable(Some(&e)).unwrap());
    }
}

#[test]
fn test_the_shim_names_match_this_macs_usr_bin() {
    // Every program in /usr/bin that is git's file (or xcodebuild's) under another name is in the lists, so the spawn
    // scan looks for all of them. One way only: another macOS may have fewer.
    assert!(SHIMS.len() == 78 && XCODE_SHIMS.contains("xcrun"));
    assert_eq!(XCODE_SHIMS.len(), 17);
    if sys_platform() != "darwin" || !Path::new("/usr/bin/git").exists() {
        eprintln!("skipped: not a Mac with /usr/bin/git");
        return;
    }
    for (anchor, names) in [("git", &*SHIMS), ("xcodebuild", &*XCODE_SHIMS)] {
        let anchor_path = format!("/usr/bin/{anchor}");
        let Ok(st) = std::fs::metadata(&anchor_path) else { continue };
        let mut same = std::collections::BTreeSet::new();
        for entry in std::fs::read_dir("/usr/bin").unwrap().flatten() {
            let Ok(m) = std::fs::metadata(entry.path()) else { continue };
            if m.is_file() && m.ino() == st.ino() && m.dev() == st.dev() {
                same.insert(entry.file_name().to_string_lossy().into_owned());
            }
        }
        assert!(same.contains(anchor)); // positive control: the scan finds the anchor
        let missing: Vec<&String> = same.iter().filter(|n| !names.contains(n.as_str())).collect();
        assert!(missing.is_empty(), "{missing:?}");
    }
}
