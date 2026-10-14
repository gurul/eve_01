"""Developer tools on this Mac, found from the files they live in, without starting anything.

macOS keeps 78 programs in /usr/bin that are one binary under 78 names (hard links of one file, ``SHIMS``): git,
clang, cc, make, python3, install_name_tool, swift and the rest. Each one looks for the developer tools (Xcode, or the
Command Line Tools) and runs their copy of itself; finding none, it opens the "install command line developer tools"
dialog and runs nothing. Sixteen more (xcodebuild, xctrace, ``XCODE_SHIMS``) are a second such binary for Xcode itself,
and xcrun is the dispatcher both kinds call. So "is there a git" is not ``which git``: on a Mac without the tools,
/usr/bin/git is there, and running it is the dialog. eve lite is for exactly those Macs (docs/plans/eve-lite.md M1).

``dev_tools_present()`` answers from the places xcode-select looks, in its order: ``$DEVELOPER_DIR``, the choice
``xcode-select -s`` saved (/var/db/xcode_select_link), Xcode in /Applications, the Command Line Tools. It starts no
process: asking ``xcode-select -p`` or ``xcrun`` is itself one of the ways to raise the dialog.

``git_usable()`` is whether running "git" runs git. In eve lite it is False (its memory keeps no git history, and its
process firewall refuses git anyway, profile.py). Off macOS it is True: there is no shim there to avoid, /usr/bin/git
is git on Linux, and git runs as it always has (a missing git still fails as FileNotFoundError, an OSError every
caller reads as "no git"). On macOS it is the git a subprocess would find on the PATH, followed through symlinks: a
git outside /usr/bin (Homebrew's) is git; /usr/bin/git, or a link to it, only while the developer tools are there.
records.py asks before every git it runs, so on the owner's Mac (Xcode installed) and off macOS nothing changes, and
full eve on a Mac without the tools keeps its records without history instead of opening the dialog.
"""

from __future__ import annotations

import glob
import os
import sys
from typing import Any, Mapping, Optional

from . import profile

SHIM_DIR = "/usr/bin"
# The Command Line Tools shim: one file under these 78 names in /usr/bin on macOS 27.0 (every name sharing
# /usr/bin/git's inode, counted 2026-10-13). test_toolchain.py checks this Mac's /usr/bin against the list.
SHIMS: frozenset[str] = frozenset({
    "ar", "as", "asa", "bison", "bm4", "c++", "c++filt", "c89", "c99", "cc", "clang", "clang++", "clangd",
    "cmpdylib", "codesign_allocate", "cpp", "ctags", "ctf_insert", "DeRez", "dsymutil", "dwarfdump", "dyld_info",
    "flex", "flex++", "g++", "gatherheaderdoc", "gcc", "gcov", "GetFileInfo", "git", "git-receive-pack",
    "git-shell", "git-upload-archive", "git-upload-pack", "gm4", "gnumake", "gperf", "hdxml2manxml",
    "headerdoc2html", "indent", "install_name_tool", "ld", "lex", "libtool", "lipo", "lldb", "llvm-g++",
    "llvm-gcc", "lorder", "m4", "make", "mig", "nm", "nmedit", "objdump", "otool", "pagestuff", "pip3", "python3",
    "ranlib", "ResMerger", "resolveLinks", "Rez", "rpcgen", "segedit", "SetFile", "size", "sourcekit-lsp",
    "SplitForks", "strings", "strip", "swift", "swiftc", "unifdef", "unifdefall", "vtool", "xml2man", "yacc",
})
# Xcode's own shim (the 16 names sharing /usr/bin/xcodebuild's inode, same count), and xcrun, which both shims call.
XCODE_SHIMS: frozenset[str] = frozenset({
    "actool", "agvtool", "desdp", "genstrings", "ibtool", "ictool", "opendiff", "sdef", "sdp", "stapler", "xcdebug",
    "xcodebuild", "xcscontrol", "xcsdiagnose", "xctrace", "xed", "xcrun",
})

DEVELOPER_DIR_ENV = "DEVELOPER_DIR"
SELECT_LINK = "/var/db/xcode_select_link"                 # written by `xcode-select -s`; absent until someone does
XCODE_GLOB = "/Applications/Xcode*.app/Contents/Developer"
CLT_DIR = "/Library/Developer/CommandLineTools"
GIT_IN_DEVELOPER_DIR = os.path.join("usr", "bin", "git")


def _program(path: str) -> bool:
    """A file the kernel would run: a regular file with the execute bit for this user."""
    return os.path.isfile(path) and os.access(path, os.X_OK)


def developer_dirs(environ: Optional[Mapping[str, str]] = None) -> list[str]:
    """Every developer directory a shim may use here, in xcode-select's order of preference. Nothing is run."""
    env = os.environ if environ is None else environ
    dirs: list[str] = []
    chosen = str(env.get(DEVELOPER_DIR_ENV) or "").strip()
    if chosen:
        dirs += [chosen, os.path.join(chosen, "Contents", "Developer")]   # xcrun takes an Xcode.app or its Developer
    try:
        link = os.readlink(SELECT_LINK)
    except OSError:
        link = ""
    if link:
        dirs.append(os.path.join(os.path.dirname(SELECT_LINK), link))
    dirs += sorted(glob.glob(XCODE_GLOB))
    dirs.append(CLT_DIR)
    return dirs


def dev_tools_present(environ: Optional[Mapping[str, str]] = None) -> bool:
    """Whether a developer directory with git in it is installed, so the /usr/bin shims run instead of asking to
    install one. Starts no process (xcode-select and xcrun are shims' kin: asking them can raise the dialog)."""
    return any(_program(os.path.join(d, GIT_IN_DEVELOPER_DIR)) for d in developer_dirs(environ))


def which(name: str, environ: Optional[Mapping[str, Any]] = None) -> Optional[str]:
    """The file a subprocess would run for the bare ``name`` under ``environ``: the first program on its PATH, with
    subprocess's own rules (os.get_exec_path: an unset PATH is os.defpath, an empty entry is the working folder).
    None when there is none. shutil.which is not used: its fallbacks differ from what subprocess runs."""
    try:
        dirs = os.get_exec_path(environ)
    except ValueError:                        # both PATH and b"PATH" set: subprocess refuses such an environment
        return None
    for d in dirs:
        candidate = os.path.join(os.fsdecode(d), name)
        if _program(candidate):
            return candidate
    return None


def is_shim(path: str) -> bool:
    """Whether ``path``, followed through symlinks, sits in /usr/bin, where git is always the shim. Compared without
    case: the system volume is case-insensitive, so /USR/BIN/git is the same file."""
    real = os.path.realpath(path)
    return os.path.dirname(real).casefold() == os.path.realpath(SHIM_DIR).casefold()


def git_usable(environ: Optional[Mapping[str, Any]] = None) -> bool:
    """Whether running "git" with ``environ`` (default: this process's) runs git: never in eve lite; always off macOS,
    where no shim stands in for it; on macOS a git outside /usr/bin always, and /usr/bin/git, or a link to it, only
    while the developer tools are present."""
    env = os.environ if environ is None else environ
    if profile.is_lite(env):
        return False
    if sys.platform != "darwin":
        return True
    found = which("git", env)
    if found is None:
        return False
    return dev_tools_present(env) if is_shim(found) else True
