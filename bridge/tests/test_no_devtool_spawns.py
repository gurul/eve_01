"""Every place eve's code names a developer-tool shim as a program to run is known (docs/plans/eve-lite.md WP-2).

On a Mac without Xcode's tools, running any of /usr/bin's shims (toolchain.SHIMS: git, clang, make, python3,
install_name_tool, ... and Xcode's own, toolchain.XCODE_SHIMS) opens the "install developer tools" dialog. The scan
reads eve_bridge's source for the shapes a program start takes: an argv literal (``["git", "-C", ...]``), a shell or
exec call's first argument (``os.system("git status")``, ``create_subprocess_exec("clang", ...)``), ``shutil.which("x")``
and a ``/usr/bin/<shim>`` path. Each site found must be in KNOWN_SITES with why it cannot open the dialog in eve lite;
a new one fails here until someone says why (or guards it with toolchain.git_usable / profile.is_lite). The control:
the OBSBOT helper's clang, obsbot_usb.py line 60 at d419b37, must be found.
"""

from __future__ import annotations

import ast
from pathlib import Path
from typing import Optional

from eve_bridge import toolchain

PKG = Path(__file__).resolve().parents[1] / "src" / "eve_bridge"
SHIM_NAMES = toolchain.SHIMS | toolchain.XCODE_SHIMS
SHIM_DIR = toolchain.SHIM_DIR + "/"
# The calls that start a program from their first argument: subprocess's, asyncio's, os's exec/spawn/system/popen.
STARTS = frozenset({
    "run", "Popen", "call", "check_call", "check_output", "getoutput", "getstatusoutput", "system", "popen",
    "create_subprocess_exec", "create_subprocess_shell", "execv", "execve", "execvp", "execvpe", "execl", "execle",
    "execlp", "execlpe", "spawnv", "spawnve", "spawnvp", "spawnvpe", "spawnl", "spawnle", "spawnlp", "spawnlpe",
    "posix_spawn", "posix_spawnp",
})
# Where an argv literal is one: an argument, an assignment, a return value (through + and * around it).
_ARGV_PARENTS = (ast.Call, ast.keyword, ast.Assign, ast.AnnAssign, ast.Return)

# (file, shim) -> (how many functions there name it, why none can open the developer-tools dialog in eve lite).
# Counted per file, not by function name, so a rename elsewhere in a file does not fail this; a new site does.
KNOWN_SITES: dict[tuple[str, str], tuple[int, str]] = {
    ("daemon.py", "git"): (1, "code_origin: full eve's start-up line naming its checkout; lite never imports "
                              "daemon.py"),
    ("obsbot_usb.py", "clang"): (1, "build: the OBSBOT camera's helper; EVE_OBSBOT is forced off in lite, and its "
                                    "firewall refuses clang"),
    ("records.py", "git"): (3, "_git runs every git the records need, after toolchain.git_usable (False in lite, and "
                               "for the shim without the tools); ensure_repo and squash_history only look it up"),
    ("self_context.py", "git"): (1, "the running commit: answered '' in lite before any process starts"),
    ("toolchain.py", "git"): (1, "git_usable looks git up on the PATH (toolchain.which): it starts nothing"),
    ("update.py", "git"): (3, "eve-bridge update, a checkout's git pull, which lite refuses"),
}


def _shim(value: object, *, command: bool = False) -> Optional[str]:
    """The shim a string names as a program: "git" or "/usr/bin/git"; with ``command``, a shell command's first word
    too ("git status"). None for anything else."""
    if not isinstance(value, str):
        return None
    word = value.strip().split(None, 1)[0] if command and value.strip() else value
    if word.startswith(SHIM_DIR):
        word = word[len(SHIM_DIR):]
    elif "/" in word:
        return None
    return word if word in SHIM_NAMES else None


def _first_text(node: ast.AST) -> object:
    """A call argument's text when it is written in the source: a string, or an f-string's fixed start."""
    if isinstance(node, ast.Constant):
        return node.value
    if isinstance(node, ast.JoinedStr) and node.values and isinstance(node.values[0], ast.Constant):
        return node.values[0].value
    return None


def scan(tree: ast.AST) -> list[tuple[str, str, int]]:
    """(function, shim, line) for each place the source names a shim as a program."""
    found: list[tuple[str, str, int]] = []
    parents: dict[ast.AST, ast.AST] = {}
    for parent in ast.walk(tree):
        for child in ast.iter_child_nodes(parent):
            parents[child] = parent

    def where(node: ast.AST) -> str:
        names: list[str] = []
        at = parents.get(node)
        while at is not None:
            if isinstance(at, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
                names.append(at.name)
            at = parents.get(at)
        return ".".join(reversed(names)) or "<module>"

    def is_argv(node: ast.AST) -> bool:
        at = parents.get(node)
        while isinstance(at, (ast.BinOp, ast.Starred)):
            at = parents.get(at)
        return isinstance(at, _ARGV_PARENTS)

    for node in ast.walk(tree):
        shim = None
        if isinstance(node, (ast.List, ast.Tuple)) and node.elts and is_argv(node):
            shim = _shim(_first_text(node.elts[0]))
        elif isinstance(node, ast.Call) and node.args:
            func = node.func
            name = func.attr if isinstance(func, ast.Attribute) else func.id if isinstance(func, ast.Name) else ""
            if name == "which":
                shim = _shim(_first_text(node.args[0]))
            elif name in STARTS:
                shim = _shim(_first_text(node.args[0]), command=True)
        elif isinstance(node, ast.Constant) and isinstance(node.value, str) and node.value.startswith(SHIM_DIR):
            shim = _shim(node.value)
        if shim:
            found.append((where(node), shim, node.lineno))
    return found


def sites() -> dict[tuple[str, str], dict[str, list[int]]]:
    """Every site in eve_bridge: (file, shim) -> {function: the lines it names the shim on}."""
    out: dict[tuple[str, str], dict[str, list[int]]] = {}
    for path in sorted(PKG.rglob("*.py")):
        for function, shim, line in scan(ast.parse(path.read_text(encoding="utf-8"))):
            out.setdefault((path.relative_to(PKG).as_posix(), shim), {}).setdefault(function, []).append(line)
    return out


def test_every_developer_tool_site_is_known() -> None:
    found = sites()
    wrong = {f"{f} names {shim} in {len(functions)} function(s), {sorted(functions)} (known: "
             f"{KNOWN_SITES.get((f, shim), (0, ''))[0]})"
             for (f, shim), functions in found.items() if len(functions) != KNOWN_SITES.get((f, shim), (0, ""))[0]}
    gone = sorted(f"{f} {shim}" for (f, shim) in KNOWN_SITES if (f, shim) not in found)
    assert not wrong, ("a new place names a developer-tool shim as a program: guard it (toolchain.git_usable, "
                       f"profile.is_lite) and record it in KNOWN_SITES with why: {sorted(wrong)}")
    assert not gone, f"KNOWN_SITES names a site that is no longer there; take it out: {gone}"


def test_the_obsbot_helpers_clang_is_found() -> None:
    """The positive control: obsbot_usb.build looks clang up (line 60 at d419b37), so a scan that found nothing
    would fail here, not pass."""
    source = (PKG / "obsbot_usb.py").read_text(encoding="utf-8")
    line = next(i for i, text in enumerate(source.splitlines(), 1) if 'shutil.which("clang")' in text)
    assert line in sites().get(("obsbot_usb.py", "clang"), {}).get("build", [])


def test_the_scan_sees_each_shape_a_start_takes() -> None:
    tree = ast.parse('''
import asyncio, os, shutil, subprocess
from pathlib import Path

def argv():
    subprocess.run(["git", "status"])
def argv_plus(extra):
    return ["xcrun", "simctl"] + extra
def argv_tuple():
    cmd = ("/usr/bin/make", "-j4")
def shell():
    os.system("swift build")
def shell_f(path):
    subprocess.run(f"python3 {path}", shell=True)
async def exec_args():
    await asyncio.create_subprocess_exec("clang", "-O2")
def which():
    return shutil.which("install_name_tool")
def a_path():
    return Path("/usr/bin/lldb").exists()
class Holder:
    def method(self):
        os.execvp("xcodebuild", ["xcodebuild"])

def not_starts(value):
    for key, label in (("cc", "Cc"), ("bcc", "Bcc")):
        pass
    words = {"git": 1}
    note = "git is not installed"
    other = ["gits", "make-believe"]
    elsewhere = "/opt/homebrew/bin/git"
    print("make sure", "/usr/bin/gitx")
''')
    assert sorted((fn, shim) for fn, shim, _line in scan(tree)) == sorted([
        ("argv", "git"), ("argv_plus", "xcrun"), ("argv_tuple", "make"), ("argv_tuple", "make"), ("shell", "swift"),
        ("shell_f", "python3"), ("exec_args", "clang"), ("which", "install_name_tool"), ("a_path", "lldb"),
        ("Holder.method", "xcodebuild"), ("Holder.method", "xcodebuild")])


def test_the_shim_names_are_the_developer_tools() -> None:
    assert {"git", "clang", "cc", "make", "python3", "pip3", "install_name_tool", "swift", "lldb"} <= toolchain.SHIMS
    assert {"xcodebuild", "xcrun"} <= toolchain.XCODE_SHIMS
    assert not {"pmset", "scutil", "ifconfig", "osascript", "open", "sh"} & SHIM_NAMES
