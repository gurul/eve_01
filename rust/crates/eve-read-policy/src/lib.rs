//! Containment check for Read-tool prompts.
//!
//! Claude Code auto-allows reads inside the session's working directory, so
//! those never prompt anywhere. [`is_within`] is the purely lexical check that
//! `daemon._handle_read_pretooluse` uses to recognise an in-cwd read and defer
//! it without a card.
//!
//! Port of `eve_bridge/read_policy.py`. The Python is
//! `Path(os.path.normpath(Path(x).expanduser()))` on both sides, then
//! `p == b or b in p.parents`; this module reproduces pathlib's parsing,
//! `Path.expanduser`, `os.path.expanduser` and `posixpath.normpath` exactly,
//! including the `//` root POSIX keeps distinct from `/`.

use std::ffi::{CStr, CString};

/// Where `~` and `~user` resolve. The real lookup reads `HOME` and the
/// password database; tests pass their own so they never touch the process
/// environment.
pub trait HomeLookup {
    /// `$HOME` if the variable is set (even to an empty string), else `None`.
    fn home_env(&self) -> Option<String>;
    /// The current user's home from the password database (`getpwuid`).
    fn current_user_home(&self) -> Option<String>;
    /// A named user's home from the password database (`getpwnam`). `None`
    /// when the user does not exist or the name cannot be looked up.
    fn user_home(&self, name: &str) -> Option<String>;
}

/// The process environment and the system password database.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemHome;

impl HomeLookup for SystemHome {
    fn home_env(&self) -> Option<String> {
        std::env::var_os("HOME").map(|v| v.to_string_lossy().into_owned())
    }

    fn current_user_home(&self) -> Option<String> {
        // SAFETY: getuid never fails.
        let uid = unsafe { libc::getuid() };
        passwd_dir(|pwd, buf, len, result| unsafe {
            libc::getpwuid_r(uid, pwd, buf, len, result)
        })
    }

    fn user_home(&self, name: &str) -> Option<String> {
        // Python's pwd.getpwnam raises ValueError on an embedded NUL; either
        // way expansion fails and the read is not inside.
        let cname = CString::new(name).ok()?;
        passwd_dir(|pwd, buf, len, result| unsafe {
            libc::getpwnam_r(cname.as_ptr(), pwd, buf, len, result)
        })
    }
}

/// Run a reentrant passwd lookup, growing the buffer on ERANGE, and return
/// `pw_dir`.
fn passwd_dir(
    call: impl Fn(*mut libc::passwd, *mut libc::c_char, libc::size_t, *mut *mut libc::passwd) -> libc::c_int,
) -> Option<String> {
    let mut size: usize = 1024;
    loop {
        let mut buf: Vec<libc::c_char> = vec![0; size];
        // SAFETY: passwd is plain old data; zeroed is a valid initial state.
        let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        let rc = call(&mut pwd, buf.as_mut_ptr(), buf.len(), &mut result);
        if rc == libc::ERANGE && size < (1 << 20) {
            size *= 2;
            continue;
        }
        if rc != 0 || result.is_null() || pwd.pw_dir.is_null() {
            return None;
        }
        // SAFETY: on success pw_dir points into `buf`, NUL-terminated.
        let dir = unsafe { CStr::from_ptr(pwd.pw_dir) };
        return Some(dir.to_string_lossy().into_owned());
    }
}

/// True if `path` is inside (or equal to) `base`. Purely lexical — no
/// filesystem access, so it cannot block the event loop or leak reads. `..`
/// is folded first (normpath): Path keeps it, so `proj/../../etc` sat "in" proj.
pub fn is_within(path: &str, base: &str) -> bool {
    is_within_with(path, base, &SystemHome)
}

/// [`is_within`] with an explicit home lookup for `~` expansion.
pub fn is_within_with(path: &str, base: &str, home: &dyn HomeLookup) -> bool {
    if path.is_empty() || base.is_empty() {
        return false;
    }
    let (Some(p), Some(b)) = (normalize(path, home), normalize(base, home)) else {
        // Python: expanduser raised RuntimeError (or getpwnam ValueError).
        return false;
    };
    let (p_root, p_parts) = pathlib_parse(&p);
    let (b_root, b_parts) = pathlib_parse(&b);
    if p_root.is_empty() || b_root.is_empty() {
        return false;
    }
    // `p == b`: posix PurePath equality is equality of the string form, which
    // for a normpath result is the string itself.
    if p == b {
        return true;
    }
    // `b in p.parents`: same root, and b's parts a proper prefix of p's.
    p_root == b_root && b_parts.len() < p_parts.len() && p_parts[..b_parts.len()] == b_parts[..]
}

/// `os.path.normpath(Path(s).expanduser())` as a string, or `None` where
/// Python raises.
fn normalize(s: &str, home: &dyn HomeLookup) -> Option<String> {
    let expanded = path_expanduser(s, home)?;
    Some(normpath(&expanded))
}

/// pathlib's POSIX parse: the root is `//` for exactly two leading slashes,
/// `/` for one or three-plus, empty when relative; parts drop empty and `.`.
fn pathlib_parse(s: &str) -> (&'static str, Vec<&str>) {
    let root = if s.starts_with("//") && !s.starts_with("///") {
        "//"
    } else if s.starts_with('/') {
        "/"
    } else {
        ""
    };
    let parts = s.split('/').filter(|c| !c.is_empty() && *c != ".").collect();
    (root, parts)
}

/// `str(Path(root, *parts))`.
fn pathlib_str(root: &str, parts: &[&str]) -> String {
    let joined = parts.join("/");
    if root.is_empty() && joined.is_empty() {
        ".".to_string()
    } else {
        format!("{root}{joined}")
    }
}

/// `str(Path(s).expanduser())`. pathlib expands only a first part starting
/// with `~` on a relative path, after it has collapsed slashes and dropped
/// `.` parts (so `./~/x` expands too). `None` where Python raises
/// `RuntimeError("Could not determine home directory.")`.
fn path_expanduser(s: &str, home: &dyn HomeLookup) -> Option<String> {
    let (root, parts) = pathlib_parse(s);
    match parts.first() {
        Some(first) if root.is_empty() && first.starts_with('~') => {
            let homedir = os_path_expanduser(first, home);
            if homedir.starts_with('~') {
                return None;
            }
            let (h_root, mut all) = pathlib_parse(&homedir);
            all.extend_from_slice(&parts[1..]);
            Some(pathlib_str(h_root, &all))
        }
        _ => Some(pathlib_str(root, &parts)),
    }
}

/// `posixpath.expanduser` for a string that starts with `~`. Returns the
/// input unchanged when the home cannot be found, as Python does.
fn os_path_expanduser(path: &str, home: &dyn HomeLookup) -> String {
    if !path.starts_with('~') {
        return path.to_string();
    }
    let i = path[1..].find('/').map_or(path.len(), |n| n + 1);
    let userhome = if i == 1 {
        match home.home_env() {
            Some(h) => h,
            None => match home.current_user_home() {
                Some(h) => h,
                None => return path.to_string(),
            },
        }
    } else {
        match home.user_home(&path[1..i]) {
            Some(h) => h,
            None => return path.to_string(),
        }
    };
    let joined = format!("{}{}", userhome.trim_end_matches('/'), &path[i..]);
    if joined.is_empty() {
        "/".to_string()
    } else {
        joined
    }
}

/// `posixpath.normpath`.
fn normpath(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let initial_slashes = if path.starts_with("//") && !path.starts_with("///") {
        2
    } else if path.starts_with('/') {
        1
    } else {
        0
    };
    let mut new_comps: Vec<&str> = Vec::new();
    for comp in path.split('/') {
        if comp.is_empty() || comp == "." {
            continue;
        }
        if comp != ".."
            || (initial_slashes == 0 && new_comps.is_empty())
            || new_comps.last() == Some(&"..")
        {
            new_comps.push(comp);
        } else if !new_comps.is_empty() {
            new_comps.pop();
        }
    }
    let out = format!("{}{}", "/".repeat(initial_slashes), new_comps.join("/"));
    if out.is_empty() {
        ".".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed home table so tests never read or change the process env.
    struct FakeHome {
        env: Option<&'static str>,
        current: Option<&'static str>,
        users: &'static [(&'static str, &'static str)],
    }

    impl HomeLookup for FakeHome {
        fn home_env(&self) -> Option<String> {
            self.env.map(str::to_string)
        }
        fn current_user_home(&self) -> Option<String> {
            self.current.map(str::to_string)
        }
        fn user_home(&self, name: &str) -> Option<String> {
            self.users.iter().find(|(n, _)| *n == name).map(|(_, h)| h.to_string())
        }
    }

    const HOME: FakeHome = FakeHome {
        env: Some("/Users/a/"),
        current: Some("/Users/pw"),
        users: &[("bob", "/Users/bob")],
    };

    // ---- is_within (test_read_policy.py) ----

    #[test]
    fn test_within_direct_child() {
        assert!(is_within("/a/b/c.txt", "/a/b"));
    }

    #[test]
    fn test_within_equal_path() {
        assert!(is_within("/a/b", "/a/b"));
    }

    #[test]
    fn test_not_within_sibling() {
        assert!(!is_within("/a/bc/file", "/a/b")); // prefix trap: /a/bc is not in /a/b
    }

    #[test]
    fn test_not_within_parent() {
        assert!(!is_within("/a", "/a/b"));
    }

    #[test]
    fn test_relative_paths_rejected() {
        assert!(!is_within("b/c.txt", "/a"));
        assert!(!is_within("/a/b", "b"));
    }

    #[test]
    fn test_empty_rejected() {
        assert!(!is_within("", "/a"));
        assert!(!is_within("/a", ""));
    }

    #[test]
    fn test_dotdot_that_climbs_out_is_not_within() {
        // review A5 (2026-10-18): `..` was never folded, so this read deferred with no card
        assert!(!is_within("/Users/a/proj/../../../etc/passwd", "/Users/a/proj"));
        assert!(!is_within("/Users/a/proj/sub/../../other/x", "/Users/a/proj"));
        assert!(is_within("/Users/a/proj/sub/../x.txt", "/Users/a/proj"));
        assert!(is_within("/Users/a/proj/x.txt", "/Users/a/proj/sub/.."));
    }

    // ---- parity edges (checked against CPython 3.11+) ----

    #[test]
    fn tilde_expands_from_home_env() {
        assert!(is_within_with("~/proj/x.txt", "/Users/a/proj", &HOME));
        assert!(is_within_with("/Users/a/proj/x", "~/proj", &HOME));
        assert!(!is_within_with("~/other", "/Users/a/proj", &HOME));
    }

    #[test]
    fn tilde_after_dot_part_still_expands() {
        // pathlib drops `.` before expanduser looks at the first part.
        assert!(is_within_with(".//~/proj/x", "/Users/a", &HOME));
    }

    #[test]
    fn tilde_falls_back_to_password_database() {
        let h = FakeHome { env: None, current: Some("/Users/pw"), users: &[] };
        assert!(is_within_with("~/x", "/Users/pw", &h));
    }

    #[test]
    fn tilde_with_no_home_is_not_within() {
        let h = FakeHome { env: None, current: None, users: &[] };
        assert!(!is_within_with("~/x", "/", &h));
    }

    #[test]
    fn empty_home_env_means_root() {
        let h = FakeHome { env: Some(""), current: None, users: &[] };
        assert!(is_within_with("~/x", "/", &h));
        assert!(is_within_with("~", "/", &h));
    }

    #[test]
    fn named_user_tilde() {
        assert!(is_within_with("~bob/notes", "/Users/bob", &HOME));
        assert!(!is_within_with("~nosuchuserxyz/notes", "/", &HOME));
    }

    #[test]
    fn tilde_not_first_is_literal() {
        assert!(!is_within_with("a/~/x", "/Users/a", &HOME));
        assert!(is_within_with("/p/~/x", "/p/~", &HOME));
    }

    #[test]
    fn double_slash_root_is_distinct() {
        // POSIX keeps exactly two leading slashes; pathlib compares them apart.
        assert!(!is_within("//a/b", "/a"));
        assert!(is_within("//a/b", "//a"));
        assert!(is_within("///a/b", "/a"));
    }

    #[test]
    fn root_base_contains_everything_absolute() {
        assert!(is_within("/etc/passwd", "/"));
        assert!(is_within("/", "/"));
        assert!(is_within("/..", "/"));
    }

    #[test]
    fn trailing_slashes_and_dots_fold() {
        assert!(is_within("/a/b/./c/", "/a/b/"));
        assert!(is_within("/a//b", "/a/b"));
    }

    #[test]
    fn normpath_matches_posixpath() {
        assert_eq!(normpath(""), ".");
        assert_eq!(normpath("a/../.."), "..");
        assert_eq!(normpath("../a/../../b"), "../../b");
        assert_eq!(normpath("/../a"), "/a");
        assert_eq!(normpath("//a/../b"), "//b");
        assert_eq!(normpath("///a/./b/"), "/a/b");
        assert_eq!(normpath("a/.."), ".");
    }

    #[test]
    fn system_home_lookup_runs() {
        // The real lookup must not panic; ~ with a real HOME resolves absolute.
        let _ = SystemHome.current_user_home();
        assert!(SystemHome.user_home("no-such-user-eve-xyz").is_none());
        assert!(SystemHome.user_home("bad\0name").is_none());
        assert!(!is_within("~bad\0n/x", "/"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn system_named_user_resolves() {
        // macOS ships `nobody` with home /var/empty; CPython 3.13 agrees.
        assert!(is_within("~nobody/x", "/var/empty"));
    }
}
