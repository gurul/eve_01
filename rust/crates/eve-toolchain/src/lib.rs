//! Developer tools on this Mac, found from the files they live in, without starting anything. Port of
//! `toolchain.py`.
//!
//! macOS keeps 78 programs in /usr/bin that are one binary under 78 names (hard links of one file, [`SHIMS`]): git,
//! clang, cc, make, python3, install_name_tool, swift and the rest. Each one looks for the developer tools (Xcode, or
//! the Command Line Tools) and runs their copy of itself; finding none, it opens the "install command line developer
//! tools" dialog and runs nothing. Sixteen more (xcodebuild, xctrace, [`XCODE_SHIMS`]) are a second such binary for
//! Xcode itself, and xcrun is the dispatcher both kinds call. So "is there a git" is not `which git`: on a Mac without
//! the tools, /usr/bin/git is there, and running it is the dialog. eve lite is for exactly those Macs
//! (docs/plans/eve-lite.md M1).
//!
//! [`dev_tools_present`] answers from the places xcode-select looks, in its order: `$DEVELOPER_DIR`, the choice
//! `xcode-select -s` saved (/var/db/xcode_select_link), Xcode in /Applications, the Command Line Tools. It starts no
//! process: asking `xcode-select -p` or `xcrun` is itself one of the ways to raise the dialog.
//!
//! [`git_usable`] is whether running "git" runs git. In eve lite it is false (its memory keeps no git history, and its
//! process firewall refuses git anyway, eve-profile). Off macOS it is true: there is no shim there to avoid,
//! /usr/bin/git is git on Linux, and git runs as it always has (a missing git still fails when spawned, an error every
//! caller reads as "no git"). On macOS it is the git a subprocess would find on the PATH, followed through symlinks: a
//! git outside /usr/bin (Homebrew's) is git; /usr/bin/git, or a link to it, only while the developer tools are there.
//! records asks before every git it runs, so on the owner's Mac (Xcode installed) and off macOS nothing changes, and
//! full eve on a Mac without the tools keeps its records without history instead of opening the dialog.
//!
//! The module-level locations ([`SHIM_DIR`], [`SELECT_LINK`], [`XCODE_GLOB`], [`CLT_DIR`]) and the platform are
//! fields of [`Toolchain`], so a test points them at a folder of its own (Python's tests monkeypatch the module
//! globals). The free functions use [`Toolchain::system`].

use std::collections::{BTreeSet, HashMap};
use std::sync::LazyLock;

use eve_profile::pypath;
use eve_profile::{EnvMap, ProcessEnv, ProfileError};

mod pyglob;

pub use pyglob::{fnmatchcase, glob};

pub const SHIM_DIR: &str = "/usr/bin";

// The Command Line Tools shim: one file under these 78 names in /usr/bin on macOS 27.0 (every name sharing
// /usr/bin/git's inode, counted 2026-10-13). The tests check this Mac's /usr/bin against the list.
const SHIM_NAMES: [&str; 78] = [
    "ar", "as", "asa", "bison", "bm4", "c++", "c++filt", "c89", "c99", "cc", "clang", "clang++", "clangd",
    "cmpdylib", "codesign_allocate", "cpp", "ctags", "ctf_insert", "DeRez", "dsymutil", "dwarfdump", "dyld_info",
    "flex", "flex++", "g++", "gatherheaderdoc", "gcc", "gcov", "GetFileInfo", "git", "git-receive-pack",
    "git-shell", "git-upload-archive", "git-upload-pack", "gm4", "gnumake", "gperf", "hdxml2manxml",
    "headerdoc2html", "indent", "install_name_tool", "ld", "lex", "libtool", "lipo", "lldb", "llvm-g++",
    "llvm-gcc", "lorder", "m4", "make", "mig", "nm", "nmedit", "objdump", "otool", "pagestuff", "pip3", "python3",
    "ranlib", "ResMerger", "resolveLinks", "Rez", "rpcgen", "segedit", "SetFile", "size", "sourcekit-lsp",
    "SplitForks", "strings", "strip", "swift", "swiftc", "unifdef", "unifdefall", "vtool", "xml2man", "yacc",
];
// Xcode's own shim (the 16 names sharing /usr/bin/xcodebuild's inode, same count), and xcrun, which both shims call.
const XCODE_SHIM_NAMES: [&str; 17] = [
    "actool", "agvtool", "desdp", "genstrings", "ibtool", "ictool", "opendiff", "sdef", "sdp", "stapler", "xcdebug",
    "xcodebuild", "xcscontrol", "xcsdiagnose", "xctrace", "xed", "xcrun",
];

/// Python's `SHIMS` frozenset.
pub static SHIMS: LazyLock<BTreeSet<&'static str>> = LazyLock::new(|| SHIM_NAMES.into_iter().collect());
/// Python's `XCODE_SHIMS` frozenset.
pub static XCODE_SHIMS: LazyLock<BTreeSet<&'static str>> =
    LazyLock::new(|| XCODE_SHIM_NAMES.into_iter().collect());

pub const DEVELOPER_DIR_ENV: &str = "DEVELOPER_DIR";
/// Written by `xcode-select -s`; absent until someone does.
pub const SELECT_LINK: &str = "/var/db/xcode_select_link";
pub const XCODE_GLOB: &str = "/Applications/Xcode*.app/Contents/Developer";
pub const CLT_DIR: &str = "/Library/Developer/CommandLineTools";
/// `os.path.join("usr", "bin", "git")`.
pub const GIT_IN_DEVELOPER_DIR: &str = "usr/bin/git";

/// Python's `sys.platform` for this build: "darwin" on macOS, "linux" on Linux, "win32" on Windows.
pub fn sys_platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    }
}

/// `_program`: a file the kernel would run: a regular file with the execute bit for this user.
pub fn program(path: &str) -> bool {
    pypath::runnable(path)
}

/// Python's `str.strip()` with no argument: the characters `str.isspace()` calls whitespace.
fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}

/// Where the shims and the developer tools live, and which platform this is: the module's globals, gathered so a
/// test can point them at a folder of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toolchain {
    pub shim_dir: String,
    pub select_link: String,
    pub xcode_glob: String,
    pub clt_dir: String,
    /// `sys.platform`: only "darwin" has shims.
    pub platform: String,
}

impl Default for Toolchain {
    fn default() -> Self {
        Self::system()
    }
}

impl Toolchain {
    /// The real locations on this machine, and this build's platform.
    pub fn system() -> Self {
        Self {
            shim_dir: SHIM_DIR.to_string(),
            select_link: SELECT_LINK.to_string(),
            xcode_glob: XCODE_GLOB.to_string(),
            clt_dir: CLT_DIR.to_string(),
            platform: sys_platform().to_string(),
        }
    }

    /// Every developer directory a shim may use here, in xcode-select's order of preference. Nothing is run.
    pub fn developer_dirs<E: EnvMap + ?Sized>(&self, env: &E) -> Vec<String> {
        let mut dirs: Vec<String> = Vec::new();
        let raw = env.env_get(DEVELOPER_DIR_ENV).unwrap_or_default();
        let chosen = py_strip(&raw);
        if !chosen.is_empty() {
            // xcrun takes an Xcode.app or its Developer
            dirs.push(chosen.to_string());
            dirs.push(pypath::join(&pypath::join(chosen, "Contents"), "Developer"));
        }
        let link = std::fs::read_link(&self.select_link)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !link.is_empty() {
            dirs.push(pypath::join(&pypath::dirname(&self.select_link), &link));
        }
        let mut xcodes = glob(&self.xcode_glob);
        xcodes.sort();
        dirs.extend(xcodes);
        dirs.push(self.clt_dir.clone());
        dirs
    }

    /// Whether a developer directory with git in it is installed, so the /usr/bin shims run instead of asking to
    /// install one. Starts no process (xcode-select and xcrun are shims' kin: asking them can raise the dialog).
    pub fn dev_tools_present<E: EnvMap + ?Sized>(&self, env: &E) -> bool {
        self.developer_dirs(env).iter().any(|d| program(&pypath::join(d, GIT_IN_DEVELOPER_DIR)))
    }

    /// Whether `path`, followed through symlinks, sits in the shim folder, where git is always the shim. Compared
    /// without case: the system volume is case-insensitive, so /USR/BIN/git is the same file.
    pub fn is_shim(&self, path: &str) -> bool {
        let real = pypath::realpath(path);
        casefold(&pypath::dirname(&real)) == casefold(&pypath::realpath(&self.shim_dir))
    }

    /// Whether running "git" with `env` runs git: never in eve lite; always off macOS, where no shim stands in for
    /// it; on macOS a git outside /usr/bin always, and /usr/bin/git, or a link to it, only while the developer tools
    /// are present. A ProfileError when EVE_PROFILE is neither full nor lite (Python raises it from profile.is_lite).
    pub fn git_usable<E: EnvMap + ?Sized>(&self, env: &E) -> Result<bool, ProfileError> {
        if eve_profile::is_lite_in(env)? {
            return Ok(false);
        }
        if self.platform != "darwin" {
            return Ok(true);
        }
        let Some(found) = which_in("git", env) else {
            return Ok(false);
        };
        Ok(if self.is_shim(&found) { self.dev_tools_present(env) } else { true })
    }
}

/// `str.casefold()`.
fn casefold(s: &str) -> String {
    caseless::default_case_fold_str(s)
}

/// `os.get_exec_path(env)`: the PATH of `env`, else `os.defpath`, split on `os.pathsep`. Python raises ValueError
/// (and `which` answers None) only when a mapping holds both "PATH" and b"PATH", which a mapping of text names
/// cannot.
fn exec_path<E: EnvMap + ?Sized>(env: &E) -> Vec<String> {
    let sep = if cfg!(windows) { ';' } else { ':' };
    env.env_get("PATH")
        .unwrap_or_else(|| pypath::DEFPATH.to_string())
        .split(sep)
        .map(str::to_string)
        .collect()
}

/// Every developer directory a shim may use here (`env` default: this process's environment).
pub fn developer_dirs(env: Option<&HashMap<String, String>>) -> Vec<String> {
    match env {
        Some(env) => Toolchain::system().developer_dirs(env),
        None => Toolchain::system().developer_dirs(&ProcessEnv),
    }
}

/// Whether the developer tools are installed (`env` default: this process's environment). Starts no process.
pub fn dev_tools_present(env: Option<&HashMap<String, String>>) -> bool {
    match env {
        Some(env) => Toolchain::system().dev_tools_present(env),
        None => Toolchain::system().dev_tools_present(&ProcessEnv),
    }
}

/// The file a subprocess would run for the bare `name` under `env` (default: this process's environment): the first
/// program on its PATH, with subprocess's own rules (os.get_exec_path: an unset PATH is os.defpath, an empty entry is
/// the working folder). None when there is none. A `which` with other fallbacks is not used: they differ from what
/// subprocess runs.
pub fn which(name: &str, env: Option<&HashMap<String, String>>) -> Option<String> {
    match env {
        Some(env) => which_in(name, env),
        None => which_in(name, &ProcessEnv),
    }
}

/// [`which`] for any environment mapping.
pub fn which_in<E: EnvMap + ?Sized>(name: &str, env: &E) -> Option<String> {
    exec_path(env).iter().map(|d| pypath::join(d, name)).find(|candidate| program(candidate))
}

/// Whether `path`, followed through symlinks, sits in /usr/bin, where git is always the shim.
pub fn is_shim(path: &str) -> bool {
    Toolchain::system().is_shim(path)
}

/// Whether running "git" with `env` (default: this process's environment) runs git. See [`Toolchain::git_usable`].
pub fn git_usable(env: Option<&HashMap<String, String>>) -> Result<bool, ProfileError> {
    match env {
        Some(env) => Toolchain::system().git_usable(env),
        None => Toolchain::system().git_usable(&ProcessEnv),
    }
}

/// [`git_usable`] for any environment mapping.
pub fn git_usable_in<E: EnvMap + ?Sized>(env: &E) -> Result<bool, ProfileError> {
    Toolchain::system().git_usable(env)
}

#[cfg(test)]
mod tests;
