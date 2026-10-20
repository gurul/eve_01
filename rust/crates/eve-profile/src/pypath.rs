//! The few `posixpath` and `os` functions the firewall reads paths with, as CPython writes them, so a path is judged
//! the same way in both daemons: `os.path.join`, `dirname`, `basename`, `normpath`, `abspath`, `realpath` (not
//! strict), `os.get_exec_path`, `os.path.isfile` plus `os.access(X_OK)`.

use std::collections::HashMap;
use std::io;

/// `os.defpath` on POSIX: the PATH used when the environment has none.
pub const DEFPATH: &str = "/bin:/usr/bin";

/// `os.path.isabs`.
pub fn isabs(path: &str) -> bool {
    path.starts_with('/')
}

/// `os.path.join(a, b)` for two parts.
pub fn join(a: &str, b: &str) -> String {
    if b.starts_with('/') {
        return b.to_string();
    }
    if a.is_empty() || a.ends_with('/') {
        format!("{a}{b}")
    } else {
        format!("{a}/{b}")
    }
}

/// `os.path.split`.
pub fn split(path: &str) -> (String, String) {
    let i = path.rfind('/').map(|i| i + 1).unwrap_or(0);
    let (head, tail) = path.split_at(i);
    let mut head = head.to_string();
    if !head.is_empty() && head != "/".repeat(head.len()) {
        head = head.trim_end_matches('/').to_string();
    }
    (head, tail.to_string())
}

/// `os.path.dirname`.
pub fn dirname(path: &str) -> String {
    split(path).0
}

/// `os.path.basename`.
pub fn basename(path: &str) -> String {
    let i = path.rfind('/').map(|i| i + 1).unwrap_or(0);
    path[i..].to_string()
}

/// `os.path.normpath`: a leading "//" is kept (POSIX leaves its meaning to the system), three or more are one.
pub fn normpath(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let initial = if path.starts_with('/') {
        if path.starts_with("//") && !path.starts_with("///") {
            2
        } else {
            1
        }
    } else {
        0
    };
    let mut comps: Vec<&str> = Vec::new();
    for comp in path.split('/') {
        if comp.is_empty() || comp == "." {
            continue;
        }
        if comp != ".." || (initial == 0 && comps.is_empty()) || comps.last() == Some(&"..") {
            comps.push(comp);
        } else if !comps.is_empty() {
            comps.pop();
        }
    }
    let joined = comps.join("/");
    let out = format!("{}{}", "/".repeat(initial), joined);
    if out.is_empty() {
        ".".to_string()
    } else {
        out
    }
}

/// `os.getcwd()`, as text.
pub fn getcwd() -> io::Result<String> {
    Ok(std::env::current_dir()?.to_string_lossy().into_owned())
}

/// `os.path.abspath`.
pub fn abspath(path: &str) -> io::Result<String> {
    if isabs(path) {
        Ok(normpath(path))
    } else {
        Ok(normpath(&join(&getcwd()?, path)))
    }
}

/// Whether `path` is a symbolic link (`stat.S_ISLNK(os.lstat(path).st_mode)`); false when lstat fails.
pub fn is_link(path: &str) -> bool {
    std::fs::symlink_metadata(path).map(|m| m.file_type().is_symlink()).unwrap_or(false)
}

/// `os.path.realpath(path)` (strict=False): every symlink resolved, a missing part kept as written, a loop left
/// where it starts.
pub fn realpath(path: &str) -> String {
    let mut seen: HashMap<String, Option<String>> = HashMap::new();
    let (resolved, _ok) = join_realpath("", path, &mut seen);
    abspath(&resolved).unwrap_or(resolved)
}

fn join_realpath(base: &str, rest: &str, seen: &mut HashMap<String, Option<String>>) -> (String, bool) {
    let mut path = base.to_string();
    let mut rest = rest.to_string();
    if isabs(&rest) {
        rest = rest[1..].to_string();
        path = "/".to_string();
    }
    while !rest.is_empty() {
        let (name, remainder) = match rest.find('/') {
            Some(i) => (rest[..i].to_string(), rest[i + 1..].to_string()),
            None => (rest.clone(), String::new()),
        };
        rest = remainder;
        if name.is_empty() || name == "." {
            continue;
        }
        if name == ".." {
            if !path.is_empty() {
                let (head, tail) = split(&path);
                path = head;
                if tail == ".." {
                    path = join(&join(&path, ".."), "..");
                }
            } else {
                path = "..".to_string();
            }
            continue;
        }
        let newpath = join(&path, &name);
        if !is_link(&newpath) {
            path = newpath;
            continue;
        }
        if let Some(known) = seen.get(&newpath) {
            match known {
                Some(resolved) => {
                    path = resolved.clone();
                    continue;
                }
                None => return (join(&newpath, &rest), false),
            }
        }
        let target = match std::fs::read_link(&newpath) {
            Ok(t) => t.to_string_lossy().into_owned(),
            Err(_) => {
                path = newpath;
                continue;
            }
        };
        seen.insert(newpath.clone(), None);
        let (resolved, ok) = join_realpath(&path, &target, seen);
        path = resolved;
        if !ok {
            return (join(&path, &rest), false);
        }
        seen.insert(newpath, Some(path.clone()));
    }
    (path, true)
}

/// `os.get_exec_path(env)`: the PATH of `env` (the process's own when None), else `os.defpath`, split on ":".
pub fn get_exec_path(env: Option<&HashMap<String, String>>) -> Vec<String> {
    let path = match env {
        Some(env) => env.get("PATH").cloned(),
        None => std::env::var_os("PATH").map(|p| p.to_string_lossy().into_owned()),
    };
    path.unwrap_or_else(|| DEFPATH.to_string()).split(':').map(str::to_string).collect()
}

/// `os.path.isfile(p) and os.access(p, os.X_OK)`: a regular file (links followed) with the execute bit.
pub fn runnable(path: &str) -> bool {
    let regular = std::fs::metadata(path).map(|m| m.is_file()).unwrap_or(false);
    regular && access_x(path)
}

fn access_x(path: &str) -> bool {
    let Ok(c) = std::ffi::CString::new(path) else {
        return false;
    };
    // SAFETY: a valid NUL-terminated string; access(2) only reads it.
    unsafe { libc::access(c.as_ptr(), libc::X_OK) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normpath_matches_posixpath() {
        for (given, want) in [
            ("", "."),
            ("/", "/"),
            ("//", "//"),
            ("///", "/"),
            ("//a//b/", "//a/b"),
            ("/a/./b/../c", "/a/c"),
            ("/..", "/"),
            ("a/../..", ".."),
            ("../a/..", ".."),
            ("a/b/", "a/b"),
            ("./", "."),
        ] {
            assert_eq!(normpath(given), want, "{given:?}");
        }
    }

    #[test]
    fn split_join_dirname_basename() {
        assert_eq!(join("/a", "b"), "/a/b");
        assert_eq!(join("/a/", "b"), "/a/b");
        assert_eq!(join("/a", "/b"), "/b");
        assert_eq!(join("", "b"), "b");
        assert_eq!(dirname("/a/b"), "/a");
        assert_eq!(dirname("/a"), "/");
        assert_eq!(dirname("//a"), "//");
        assert_eq!(dirname("a"), "");
        assert_eq!(dirname("a//b"), "a");
        assert_eq!(basename("/a/b"), "b");
        assert_eq!(basename("/a/"), "");
    }

    #[test]
    fn realpath_follows_links_and_keeps_missing_parts() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap().to_string_lossy().into_owned();
        std::fs::create_dir(format!("{root}/real")).unwrap();
        std::os::unix::fs::symlink(format!("{root}/real"), format!("{root}/link")).unwrap();
        std::os::unix::fs::symlink("real", format!("{root}/rel")).unwrap();
        assert_eq!(realpath(&format!("{root}/link/x/y")), format!("{root}/real/x/y"));
        assert_eq!(realpath(&format!("{root}/rel")), format!("{root}/real"));
        std::os::unix::fs::symlink(format!("{root}/b"), format!("{root}/a")).unwrap();
        std::os::unix::fs::symlink(format!("{root}/a"), format!("{root}/b")).unwrap();
        assert_eq!(realpath(&format!("{root}/a/f")), format!("{root}/a/f"));
    }
}
