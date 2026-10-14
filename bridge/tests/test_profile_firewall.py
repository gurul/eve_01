"""profile.install_firewall: eve lite's process firewall (docs/plans/eve-lite.md M1, WP-2).

Every case runs in a fresh Python process: an audit hook cannot be removed, so this process never installs one. The
programs a case tries are fakes in a folder of the test's own (each writes a mark if it ever runs), and the protected
folders are under a fake HOME: nothing real is started or opened, even if the firewall were broken. Real paths
(/usr/bin/git, /usr/bin/osascript, ...) are judged in this process by the firewall's own check, without the hook.
Each refusal case has its control: the same script under full eve (no firewall) runs the fake, or opens the file.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import textwrap
from pathlib import Path

import pytest

from eve_bridge import profile

SRC = Path(__file__).resolve().parents[1] / "src"
CANARY = "CANARY-NOT-A-KEY-7f3c"
LITE_PATH = "/usr/bin:/bin:/usr/sbin:/sbin"                    # the lite LaunchAgent's PATH (spec M8)
BLOCKED = ("git", "clang", "xcrun", "osascript", "screencapture", "codex", "claude", "cloudflared", "adb",
           "python3", "make", "install_name_tool")

HARNESS = '''
import json, logging, os, sys
logging.basicConfig(level=logging.INFO, stream=sys.stderr, format="%(name)s: %(message)s")
from eve_bridge import profile

RESULTS = {}
HOME = os.environ["HOME"]
MARKS = os.environ["MARKS"]


def attempt(label, fn):
    try:
        value = fn()
        RESULTS[label] = {"ran": True, "value": value if isinstance(value, (str, int, bool, type(None), list)) else
                          type(value).__name__}
    except PermissionError as e:
        RESULTS[label] = {"ran": False, "refused": str(e)}
    except Exception as e:
        RESULTS[label] = {"ran": False, "error": type(e).__name__}


def main():
    for mode in [m for m in os.environ.get("FIREWALL_MODES", "").split(",") if m]:
        profile.install_firewall(mode)
{body}
    print(json.dumps({"results": RESULTS, "counts": profile.firewall_counts(),
                      "marks": sorted(os.listdir(MARKS))}))


{top}
if __name__ == "__main__":
    main()
'''


@pytest.fixture
def box(tmp_path: Path) -> dict[str, Path]:
    """A fake bin of programs that leave a mark when they run, a fake HOME with each kind of protected folder, and
    the folder the marks go to."""
    bin_dir, marks, home = tmp_path / "bin", tmp_path / "marks", tmp_path / "home"
    for d in (bin_dir, marks):
        d.mkdir()
    for name in BLOCKED:
        fake = bin_dir / name
        fake.write_text(f'#!/bin/sh\ntouch "$MARKS/{name}"\necho RAN-{name}\n')
        fake.chmod(0o755)
    for sub in ("Documents", "Desktop", "Downloads", "Library/Messages", "Library/Mobile Documents/com~apple~CloudDocs",
                "Library/Containers/com.example.app", "Library/CloudStorage/Dropbox", ".config/eve-bridge/memory"):
        (home / sub).mkdir(parents=True)
    (home / "Documents" / "a.txt").write_text("private")
    (home / "Documents" / f"{CANARY}.txt").write_text("private")
    (home / "Downloads" / "b.txt").write_text("private")
    (home / "Library" / "CloudStorage" / "Dropbox" / "d.txt").write_text("private")
    (home / "Library" / "Mobile Documents" / "com~apple~CloudDocs" / "i.txt").write_text("private")
    (home / ".config" / "eve-bridge" / "env").write_text("EVE_PROFILE=lite\n")
    (home / ".config" / "eve-bridge" / "docs").symlink_to(home / "Documents")
    (home / "Dropbox").symlink_to(home / "Library" / "CloudStorage" / "Dropbox")
    return {"bin": bin_dir, "marks": marks, "home": home, "tmp": tmp_path}


def run(box: dict[str, Path], body: str, *, lite: bool = True, modes: str = "enforce", top: str = "",
        path: str = "") -> dict:
    """``body`` (statements inside main(), using attempt and HOME) in a fresh process, with the firewall installed in
    ``modes`` (comma-separated, in order) under EVE_PROFILE=lite, or under full eve when ``lite`` is False. What it
    returns: each attempt's result, firewall_counts, the marks the fakes left, and the process's log."""
    script = box["tmp"] / f"case-{len(list(box['tmp'].glob('case-*.py')))}.py"
    script.write_text(HARNESS.replace("{body}", textwrap.indent(textwrap.dedent(body), "    "))
                      .replace("{top}", textwrap.dedent(top)))
    env = {"PATH": path or f"{box['bin']}:{LITE_PATH}", "HOME": str(box["home"]), "PYTHONPATH": str(SRC),
           "MARKS": str(box["marks"]), "FIREWALL_MODES": modes, "TMPDIR": str(box["tmp"])}
    if lite:
        env["EVE_PROFILE"] = "lite"
    done = subprocess.run([sys.executable, str(script)], capture_output=True, text=True, env=env, timeout=120,
                          cwd=str(box["tmp"]))
    assert done.returncode == 0, done.stderr[-3000:]
    out = json.loads(done.stdout.strip().splitlines()[-1])
    out["log"] = done.stderr
    return out


# ---- programs ---------------------------------------------------------------------------------------------------

STARTS = '''
import asyncio, subprocess
for name in {names!r}:
    attempt(name, lambda name=name: subprocess.run([name], capture_output=True, text=True).stdout.strip())
attempt("shell", lambda: subprocess.run("git", shell=True, capture_output=True, text=True).stdout.strip())
attempt("os.system", lambda: os.system("git"))
attempt("os.popen", lambda: os.popen("git").read().strip())
async def exec_git():
    proc = await asyncio.create_subprocess_exec("git", stdout=asyncio.subprocess.PIPE)
    return (await proc.communicate())[0].decode().strip()
attempt("asyncio", lambda: asyncio.run(exec_git()))
attempt("posix_spawnp", lambda: os.waitpid(os.posix_spawnp("git", ["git"], dict(os.environ)), 0)[1])
attempt("posix_spawn", lambda: os.waitpid(os.posix_spawn(os.path.join(os.environ["FAKES"], "git"), ["git"],
                                                         dict(os.environ)), 0)[1])
'''


def test_developer_tools_and_the_macs_programs_are_refused(box: dict[str, Path]) -> None:
    body = "os.environ['FAKES'] = " + repr(str(box["bin"])) + "\n" + STARTS.format(names=BLOCKED)
    lite = run(box, body)
    labels = (*BLOCKED, "shell", "os.system", "os.popen", "asyncio", "posix_spawnp", "posix_spawn")
    for label in labels:
        assert lite["results"][label]["ran"] is False and "refused" in lite["results"][label], label
    assert lite["marks"] == []                                        # nothing ran, so nothing left a mark
    assert {f"exec:{name}" for name in BLOCKED} | {"exec:sh"} == set(lite["counts"])
    assert lite["counts"]["exec:git"] == 4 and lite["counts"]["exec:sh"] == 3

    full = run(box, body, lite=False)                                 # control: full eve has no firewall
    assert all(full["results"][name] == {"ran": True, "value": f"RAN-{name}"} for name in BLOCKED)
    assert full["results"]["asyncio"]["value"] == "RAN-git" and full["counts"] == {}
    assert set(full["marks"]) == set(BLOCKED)


def test_the_three_programs_it_allows_run(box: dict[str, Path]) -> None:
    """pmset, ifconfig and scutil run under the firewall, started the way lite starts them (quick_answers, watch,
    lan_net) on the LaunchAgent's PATH, and through a link to them."""
    for program in ("/usr/bin/pmset", "/sbin/ifconfig", "/usr/sbin/scutil"):
        if not os.access(program, os.X_OK):
            pytest.skip(f"{program} is not on this machine")
    links = box["tmp"] / "links"
    links.mkdir()
    (links / "pmset").symlink_to("/usr/bin/pmset")                    # a link to pmset is pmset
    body = '''
    import asyncio, subprocess
    attempt("pmset", lambda: subprocess.run(["pmset", "-g", "batt"], capture_output=True).returncode)
    attempt("ifconfig", lambda: subprocess.run(["/sbin/ifconfig", "-l"], capture_output=True).returncode)
    attempt("scutil", lambda: subprocess.run(["/usr/sbin/scutil", "--get", "LocalHostName"],
                                             capture_output=True).returncode in (0, 1))
    async def battery():                                              # quick_answers._pmset, as it runs
        proc = await asyncio.create_subprocess_exec("pmset", "-g", "batt", stdout=asyncio.subprocess.PIPE)
        await proc.communicate()
        return proc.returncode
    attempt("asyncio pmset", lambda: asyncio.run(battery()))
    attempt("linked pmset", lambda: subprocess.run([os.environ["LINKS"] + "/pmset", "-g"],
                                                   capture_output=True).returncode)
    '''
    out = run(box, "os.environ['LINKS'] = " + repr(str(links)) + "\n" + textwrap.dedent(body), path=LITE_PATH)
    assert out["results"] == {"pmset": {"ran": True, "value": 0}, "ifconfig": {"ran": True, "value": 0},
                              "scutil": {"ran": True, "value": True}, "asyncio pmset": {"ran": True, "value": 0},
                              "linked pmset": {"ran": True, "value": 0}}
    assert out["counts"] == {}


def test_a_program_is_allowed_by_its_file_not_its_name(box: dict[str, Path]) -> None:
    """A fake named pmset ahead on the PATH is not pmset: refused (and a positive control for the case above)."""
    fake = box["bin"] / "pmset"
    fake.write_text('#!/bin/sh\ntouch "$MARKS/pmset"\n')
    fake.chmod(0o755)
    out = run(box, 'import subprocess\nattempt("pmset", lambda: subprocess.run(["pmset"]).returncode)\n')
    assert out["results"]["pmset"]["ran"] is False and out["marks"] == [] and out["counts"] == {"exec:pmset": 1}


@pytest.mark.parametrize("path", ["/usr/bin/git", "/usr/bin/clang", "/usr/bin/make", "/usr/bin/python3",
                                  "/usr/bin/install_name_tool", "/usr/bin/xcrun", "/usr/bin/osascript",
                                  "/usr/sbin/screencapture", "/bin/sh", "/usr/bin/open"])
def test_the_real_programs_are_judged_refused(path: str) -> None:
    """Judged by the firewall's own check, in this process and with no hook: nothing is started."""
    if not os.path.exists(path):
        pytest.skip(f"{path} is not on this machine")
    guard = profile._Guard("enforce", [], os.getpid())
    name = os.path.basename(os.path.realpath(path))
    assert guard.check("subprocess.Popen", (path, [path], None, None))[0] == f"exec:{name}"
    assert guard.check("os.posix_spawn", (path, [path], {}))[0] == f"exec:{name}"
    assert guard.check("os.exec", (path, [path], {}))[0] == f"exec:{name}"


def test_a_start_it_cannot_read_is_refused() -> None:
    """Fails closed: an argument the guard cannot make a path of is a refusal, never a pass."""
    guard = profile._Guard("enforce", [], os.getpid())
    assert guard.check("subprocess.Popen", (object(), [], None, None))[0] == "exec:?"
    assert guard.check("os.exec", (None, [], {}))[0] == "exec:?"


def test_an_allowed_program_that_became_a_link_is_still_allowed(tmp_path: Path,
                                                                monkeypatch: pytest.MonkeyPatch) -> None:
    """A system path that a later macOS turns into a link (into a cryptex, say) is allowed by the file it reaches;
    another program in that folder is not (the control)."""
    real = tmp_path / "cryptex" / "pmset"
    real.parent.mkdir()
    real.write_text("#!/bin/sh\n")
    real.chmod(0o755)
    other = real.parent / "other"
    other.write_text("#!/bin/sh\n")
    other.chmod(0o755)
    link = tmp_path / "usr-bin" / "pmset"
    link.parent.mkdir()
    link.symlink_to(real)
    monkeypatch.setattr(profile, "EXEC_ALLOW", frozenset({str(link)}))
    guard = profile._Guard("enforce", [], os.getpid())
    assert guard.check("subprocess.Popen", (str(link), [str(link)], None, None)) is None
    assert guard.check("subprocess.Popen", ("pmset", ["pmset"], None, {"PATH": str(link.parent)})) is None
    assert guard.check("subprocess.Popen", (str(other), [str(other)], None, None))[0] == "exec:other"


@pytest.mark.parametrize("path", sorted(profile.EXEC_ALLOW))
def test_the_three_real_programs_are_judged_allowed(path: str) -> None:
    if not os.path.exists(path):
        pytest.skip(f"{path} is not on this machine")
    guard = profile._Guard("enforce", [], os.getpid())
    assert guard.check("subprocess.Popen", (path, [path], None, None)) is None
    assert guard.check("subprocess.Popen", (os.path.basename(path), [path], None, {"PATH": LITE_PATH})) is None
    assert guard.check("os.system", (b"pmset",))[0] == "exec:sh"     # a shell, whatever it would run


def test_a_forked_child_whose_exec_is_refused_exits(box: dict[str, Path]) -> None:
    body = '''
    pid = os.fork()
    if pid == 0:
        try:
            os.execv(os.path.join(os.environ["FAKES"], "git"), ["git"])
        finally:
            os._exit(99)
    attempt("child", lambda: os.waitstatus_to_exitcode(os.waitpid(pid, 0)[1]))
    '''
    script = "os.environ['FAKES'] = " + repr(str(box["bin"])) + "\n" + textwrap.dedent(body)
    lite = run(box, script)
    assert lite["results"]["child"] == {"ran": True, "value": 126} and lite["marks"] == []
    full = run(box, script, lite=False)                               # control: the exec runs
    assert full["results"]["child"] == {"ran": True, "value": 0} and full["marks"] == ["git"]


def test_multiprocessing_spawn_is_a_process_start_too(box: dict[str, Path]) -> None:
    top = '''
    def child():
        open(os.path.join(os.environ["MARKS"], "child"), "w").close()
    '''
    body = '''
    import multiprocessing
    def start():
        p = multiprocessing.get_context("spawn").Process(target=child)
        p.start()
        p.join(60)
        return p.exitcode
    attempt("spawn", start)
    '''
    lite = run(box, body, top=top)
    assert lite["results"]["spawn"]["ran"] is False and "refused" in lite["results"]["spawn"]
    python = os.path.basename(os.path.realpath(sys.executable))       # spawn starts this Python again
    assert lite["marks"] == [] and set(lite["counts"]) == {f"exec:{python}"}
    full = run(box, body, top=top, lite=False)                        # control
    assert full["results"]["spawn"] == {"ran": True, "value": 0} and full["marks"] == ["child"]


# ---- files --------------------------------------------------------------------------------------------------------

FILES = '''
import glob, pathlib, sqlite3
D = os.path.join(HOME, "Documents")
attempt("open", lambda: open(os.path.join(D, "a.txt")).read())
attempt("open, another case", lambda: open(os.path.join(HOME, "documents", "a.txt")).read())
attempt("open, through a link", lambda: open(os.path.join(HOME, ".config/eve-bridge/docs/a.txt")).read())
attempt("open, through ..", lambda: open(os.path.join(HOME, ".config/eve-bridge/../../Documents/a.txt")).read())
attempt("open, data volume", lambda: open("/System/Volumes/Data" + os.path.realpath(D) + "/a.txt").read())
attempt("open, bytes", lambda: open(os.fsencode(os.path.join(D, "a.txt")), "rb").read().decode())
attempt("pathlib", lambda: pathlib.Path(D, "a.txt").read_text())
attempt("listdir", lambda: os.listdir(os.path.join(HOME, "Desktop")))
attempt("scandir", lambda: [e.name for e in os.scandir(os.path.join(HOME, "Downloads"))])
attempt("walk", lambda: [f for _, _, fs in os.walk(D) for f in fs])            # os.walk skips what it cannot
attempt("glob", lambda: [os.path.basename(p) for p in glob.glob(os.path.join(D, "*"))])   # list, and so does glob
attempt("iCloud Drive", lambda: os.listdir(os.path.join(HOME, "Library/Mobile Documents/com~apple~CloudDocs")))
attempt("other apps' data", lambda: os.listdir(os.path.join(HOME, "Library/Containers")))
attempt("a cloud drive, through ~/Dropbox", lambda: open(os.path.join(HOME, "Dropbox", "d.txt")).read())
attempt("sqlite", lambda: sqlite3.connect(os.path.join(HOME, "Library/Messages/chat.db")).close())
attempt("sqlite uri", lambda: sqlite3.connect("file:" + os.path.join(HOME, "Library/Messages/chat.db") + "?mode=ro",
                                              uri=True).close())
attempt("mkdir", lambda: os.mkdir(os.path.join(D, "new")))
attempt("rename into", lambda: os.rename(os.path.join(HOME, ".config/eve-bridge/env"), os.path.join(D, "env")))
if os.environ.get("EVE_PROFILE") == "lite":                     # the control never lists the real /Volumes
    attempt("volumes", lambda: os.listdir("/Volumes"))
os.chdir(HOME)
attempt("relative", lambda: open("Documents/a.txt").read())
os.chdir(os.path.join(HOME, "Desktop"))
attempt("listdir here", lambda: os.listdir())
os.chdir(HOME)
attempt("eve's env file", lambda: open(os.path.join(HOME, ".config/eve-bridge/env")).read().strip())
attempt("eve's memory", lambda: open(os.path.join(HOME, ".config/eve-bridge/memory/x.md"), "w").write("ok"))
attempt("the home itself", lambda: sorted(os.listdir(HOME)) != [])
attempt("its Library", lambda: "Messages" in os.listdir(os.path.join(HOME, "Library")))
attempt("a temp file", lambda: __import__("tempfile").mkstemp()[0] > 0)
attempt("memory db", lambda: sqlite3.connect(":memory:").close())
'''
REFUSED = ("open", "open, another case", "open, through a link", "open, through ..", "open, data volume",
           "open, bytes", "pathlib", "listdir", "scandir", "iCloud Drive", "other apps' data",
           "a cloud drive, through ~/Dropbox", "sqlite", "sqlite uri", "mkdir", "rename into", "volumes", "relative",
           "listdir here")
SEE_NOTHING = ("walk", "glob")                                      # refused inside; they swallow it and list nothing
ALLOWED = ("eve's env file", "eve's memory", "the home itself", "its Library", "a temp file", "memory db")


def test_protected_folders_are_refused_and_eves_own_are_not(box: dict[str, Path]) -> None:
    lite = run(box, FILES)
    results = lite["results"]
    refused = {label for label in REFUSED if results[label]["ran"] is False and "refused" in results[label]}
    assert refused == set(REFUSED), {label: results[label] for label in REFUSED if label not in refused}
    assert all(results[label]["ran"] for label in ALLOWED), {label: results[label] for label in ALLOWED}
    assert all(results[label] == {"ran": True, "value": []} for label in SEE_NOTHING)
    assert results["eve's env file"]["value"] == "EVE_PROFILE=lite"
    assert set(lite["counts"]) == {"path:~/Documents", "path:~/Desktop", "path:~/Downloads", "path:~/Library/Messages",
                                   "path:~/Library/Mobile Documents", "path:~/Library/Containers",
                                   "path:~/Library/CloudStorage", "path:/Volumes"}
    assert (box["home"] / ".config" / "eve-bridge" / "env").exists() and not (box["home"] / "Documents" / "new").exists()

    full = run(box, FILES, lite=False)                                # control: full eve opens every one of them
    controls = [label for label in REFUSED if label != "volumes"]
    assert all(full["results"][label]["ran"] for label in controls), {
        label: full["results"][label] for label in controls if not full["results"][label]["ran"]}
    assert full["results"]["open, through a link"]["value"] == "private" and full["counts"] == {}
    assert all(full["results"][label]["value"] for label in SEE_NOTHING)


def test_a_path_is_judged_by_its_words_first() -> None:
    """Inside the folder (in any case) is refused before anything is looked at; beside it, or not a path, is not.
    Judged in this process, with no hook."""
    guard = profile._Guard("enforce", [("/nowhere/home/Documents", "~/Documents")], os.getpid())
    assert guard.reaches("/nowhere/home/Documents/a/b/c.txt") == "~/Documents"
    assert guard.reaches("/NOWHERE/home/documents") == "~/Documents"
    assert guard.reaches("/nowhere/home/Documents2/x") is None and guard.reaches("/nowhere/home") is None
    assert guard.reaches(3) is None and guard.reaches(None) is None      # a descriptor; the working folder


def test_following_a_link_stops_at_the_protected_folder(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """Every lstat the walk makes is recorded: the links outside are followed, nothing at or under ~/Documents is
    looked at, and the path is still refused. The control: a link to a folder outside is followed to its end."""
    home = tmp_path / "home"
    (home / "Documents" / "deep").mkdir(parents=True)
    (home / "Public").mkdir()
    (home / "a").symlink_to(home / "b")
    (home / "b").symlink_to(home / "Documents" / "deep")              # a chain of links into the folder
    (home / "c").symlink_to(home / "Public")
    looked: list[str] = []
    real_lstat = os.lstat

    def lstat(path: str, *args, **kwargs):
        looked.append(str(path))
        return real_lstat(path, *args, **kwargs)

    guard = profile._Guard("enforce", profile.protected_roots({"HOME": str(home)}), os.getpid())
    inside = str(home / "Documents")                                  # tmp_path is already resolved
    monkeypatch.setattr(os, "lstat", lstat)
    assert guard.reaches(str(home / "a" / "x.txt")) == "~/Documents"
    walked = list(looked)
    assert str(home / "a") in walked and str(home / "b") in walked
    assert not [p for p in walked if p == inside or p.startswith(inside + "/")], walked
    looked.clear()
    assert guard.reaches(str(home / "c" / "y.txt")) is None
    assert str(home / "Public" / "y.txt") in looked


def test_a_loop_of_links_reaches_nothing(tmp_path: Path) -> None:
    (tmp_path / "x").symlink_to(tmp_path / "y")
    (tmp_path / "y").symlink_to(tmp_path / "x")
    guard = profile._Guard("enforce", profile.protected_roots({"HOME": str(tmp_path)}), os.getpid())
    assert guard.reaches(str(tmp_path / "x" / "f")) is None


def test_this_processs_own_code_stays_importable_inside_a_protected_folder(tmp_path: Path) -> None:
    """A developer's checkout under ~/Documents: the code folders are exempt, the rest of the folder is not."""
    documents = tmp_path / "Documents"
    code = documents / "checkout" / "src"
    guard = profile._Guard("enforce", [(str(documents), "~/Documents")], os.getpid(), [str(code)])
    assert guard.reaches(str(code / "eve_bridge" / "telegram.py")) is None
    assert guard.reaches(str(code)) is None
    assert guard.reaches(str(documents / "checkout" / "notes.txt")) == "~/Documents"
    assert guard.reaches(str(documents / "checkout" / "src2" / "x.py")) == "~/Documents"
    # a code folder outside the protected ones with a link into one: exempt by its words only, so still refused
    venv = tmp_path / "venv"
    venv.mkdir()
    (venv / "docs").symlink_to(documents)
    guard = profile._Guard("enforce", [(str(documents), "~/Documents")], os.getpid(), [str(venv)])
    assert guard.reaches(str(venv / "docs" / "a.txt")) == "~/Documents"
    assert guard.reaches(str(venv / "lib" / "x.py")) is None
    roots = profile.code_roots()
    assert os.path.normpath(sys.prefix) in roots and (str(SRC) in roots or os.path.realpath(SRC) in roots)


# ---- modes, logs, counts ----------------------------------------------------------------------------------------

def test_audit_mode_lets_everything_through_and_counts_it(box: dict[str, Path]) -> None:
    body = '''
    import subprocess
    for i in range(2):
        attempt(f"git {i}", lambda: subprocess.run(["git"], capture_output=True, text=True).stdout.strip())
    attempt("open", lambda: open(os.path.join(HOME, "Documents", "a.txt")).read())
    '''
    out = run(box, body, modes="audit")
    assert out["results"] == {"git 0": {"ran": True, "value": "RAN-git"}, "git 1": {"ran": True, "value": "RAN-git"},
                              "open": {"ran": True, "value": "private"}}
    assert out["counts"] == {"exec:git": 2, "path:~/Documents": 1} and out["marks"] == ["git"]
    assert out["log"].count("would refuse the program git") == 1      # said once, counted twice


def test_enforcing_cannot_be_loosened(box: dict[str, Path]) -> None:
    body = 'import subprocess\nattempt("git", lambda: subprocess.run(["git"], capture_output=True).returncode)\n'
    for modes in ("audit,enforce", "enforce,audit", "enforce,enforce"):
        out = run(box, body, modes=modes)
        assert out["results"]["git"]["ran"] is False and out["marks"] == [], modes


def test_the_log_names_programs_and_folders_never_values(box: dict[str, Path]) -> None:
    body = f'''
    import subprocess
    attempt("git", lambda: subprocess.run(["git", "--token", "{CANARY}"], capture_output=True).returncode)
    attempt("open", lambda: open(os.path.join(HOME, "Documents", "{CANARY}.txt")).read())
    '''
    out = run(box, body)
    assert out["results"]["git"]["ran"] is False and out["results"]["open"]["ran"] is False
    lines = [line for line in out["log"].splitlines() if line.startswith("eve_bridge.profile")]
    assert any("the program git" in line for line in lines) and any("~/Documents" in line for line in lines)
    assert CANARY not in out["log"] and str(box["home"]) not in out["log"]
    assert CANARY not in json.dumps(out["results"]) and str(box["home"]) not in json.dumps(out["results"])


def test_the_standard_librarys_own_programs_are_asked_before_the_firewall(box: dict[str, Path]) -> None:
    """platform.processor() is `uname -p` and platform.platform() also runs `file`: mem0 and the openai SDK ask for
    them on a model call. Answered before the hook went in, so they trip nothing; asking afresh is refused (the
    control: the stdlib does start those programs)."""
    body = '''
    import platform
    attempt("platform", lambda: bool(platform.platform()) and bool(platform.processor()))
    attempt("fresh uname", lambda: platform._Processor.from_subprocess())
    attempt("fresh file", lambda: platform.architecture())
    '''
    out = run(box, body)
    assert out["results"]["platform"] == {"ran": True, "value": True}
    assert out["counts"] == {"exec:uname": 1, "exec:file": 1}         # only the fresh asks, after the fact


def test_counts_are_a_copy(monkeypatch: pytest.MonkeyPatch) -> None:
    assert profile.firewall_counts() == {}                            # no firewall in this process
    guard = profile._Guard("audit", [], os.getpid())                  # never added as a hook here
    guard.note("exec:git", "the program git")
    monkeypatch.setattr(profile, "_GUARD", guard)
    counts = profile.firewall_counts()
    counts["exec:git"] = 99
    assert profile.firewall_counts() == {"exec:git": 1}
