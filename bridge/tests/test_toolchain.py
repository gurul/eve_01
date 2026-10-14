"""toolchain.py: whether the developer tools are on this Mac, and whether "git" runs git, found without starting a
process; and records.py's guard, which asks before every git it runs (docs/plans/eve-lite.md M1, WP-2).

Every fake toolchain here is a folder of the test's own: the module's locations (CLT_DIR, XCODE_GLOB, SELECT_LINK,
SHIM_DIR) are pointed at it, so no test depends on what this Mac has installed, and no test runs a real shim.
"""

from __future__ import annotations

import json
import logging
import os
import subprocess
import sys
from pathlib import Path

import pytest

from eve_bridge import records, toolchain

SRC = Path(__file__).resolve().parents[1] / "src"


def program(path: Path, text: str = "#!/bin/sh\nexit 0\n") -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)
    path.chmod(0o755)
    return path


@pytest.fixture
def no_tools(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """A Mac with no developer tools anywhere xcode-select looks: every location is an empty folder of the test's. A
    Mac wherever the test runs: git_usable reads the shim rules on macOS only."""
    root = tmp_path / "mac"
    monkeypatch.setattr(toolchain.sys, "platform", "darwin")
    monkeypatch.setattr(toolchain, "CLT_DIR", str(root / "CommandLineTools"))
    monkeypatch.setattr(toolchain, "XCODE_GLOB", str(root / "Applications" / "Xcode*.app" / "Contents" / "Developer"))
    monkeypatch.setattr(toolchain, "SELECT_LINK", str(root / "db" / "xcode_select_link"))
    monkeypatch.delenv(toolchain.DEVELOPER_DIR_ENV, raising=False)
    monkeypatch.delenv("EVE_PROFILE", raising=False)
    return root


def git_in(developer_dir: Path) -> Path:
    return program(developer_dir / "usr" / "bin" / "git")


# ---- dev_tools_present: the places xcode-select looks ----------------------------------------------------------

def test_no_developer_directory_anywhere_is_no_tools(no_tools: Path) -> None:
    assert toolchain.dev_tools_present({}) is False
    assert toolchain.developer_dirs({}) == [toolchain.CLT_DIR]


def test_the_command_line_tools_count(no_tools: Path) -> None:
    git_in(Path(toolchain.CLT_DIR))
    assert toolchain.dev_tools_present({}) is True


def test_a_git_that_cannot_run_is_not_the_tools(no_tools: Path) -> None:
    git_in(Path(toolchain.CLT_DIR)).chmod(0o644)
    assert toolchain.dev_tools_present({}) is False


def test_any_xcode_in_applications_counts(no_tools: Path) -> None:
    git_in(no_tools / "Applications" / "Xcode-beta.app" / "Contents" / "Developer")
    assert toolchain.dev_tools_present({}) is True
    assert toolchain.developer_dirs({})[0].endswith("Xcode-beta.app/Contents/Developer")


def test_the_folder_xcode_select_chose_counts(no_tools: Path) -> None:
    chosen = no_tools / "Users" / "someone" / "Applications" / "Xcode.app" / "Contents" / "Developer"
    link = Path(toolchain.SELECT_LINK)
    link.parent.mkdir(parents=True)
    link.symlink_to(chosen)
    assert toolchain.dev_tools_present({}) is False             # chosen, but nothing there (control)
    git_in(chosen)
    assert toolchain.dev_tools_present({}) is True


def test_developer_dir_counts_as_xcrun_reads_it(no_tools: Path) -> None:
    xcode = no_tools / "Elsewhere" / "Xcode.app"
    git_in(xcode / "Contents" / "Developer")
    assert toolchain.dev_tools_present({toolchain.DEVELOPER_DIR_ENV: str(xcode)}) is True           # the .app
    assert toolchain.dev_tools_present(
        {toolchain.DEVELOPER_DIR_ENV: str(xcode / "Contents" / "Developer")}) is True                # its Developer
    assert toolchain.dev_tools_present({toolchain.DEVELOPER_DIR_ENV: str(no_tools / "nothing")}) is False


def test_dev_tools_present_and_git_usable_start_no_process(tmp_path: Path) -> None:
    """Asked in a fresh process with an audit hook listing every process start: none. The same hook sees a start the
    script makes afterwards (positive control), so an empty list means nothing was started."""
    script = tmp_path / "probe.py"
    script.write_text(
        "import json, subprocess, sys\n"
        "starts = []\n"
        "sys.addaudithook(lambda e, a: starts.append(e) if e in ('subprocess.Popen', 'os.posix_spawn', 'os.exec',"
        " 'os.spawn', 'os.system') else None)\n"
        "from eve_bridge import toolchain\n"
        "answers = [toolchain.dev_tools_present(), toolchain.git_usable(), toolchain.developer_dirs()]\n"
        "before = list(starts)\n"
        "subprocess.run(['/usr/bin/true'])\n"
        "print(json.dumps({'before': before, 'after': starts}))\n")
    env = {"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "HOME": str(tmp_path), "PYTHONPATH": str(SRC)}
    done = subprocess.run([sys.executable, str(script)], capture_output=True, text=True, env=env, timeout=60)
    assert done.returncode == 0, done.stderr
    seen = json.loads(done.stdout.strip().splitlines()[-1])
    assert seen["before"] == []
    assert "subprocess.Popen" in seen["after"]


# ---- which and is_shim: what a subprocess would run ------------------------------------------------------------

def test_which_follows_subprocess_rules(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    a, b = tmp_path / "a", tmp_path / "b"
    program(b / "tool")
    (a / "tool").parent.mkdir(parents=True)
    (a / "tool").write_text("not a program")                     # not executable: skipped, as exec skips it
    assert toolchain.which("tool", {"PATH": f"{a}:{b}"}) == str(b / "tool")
    assert toolchain.which("tool", {"PATH": str(a)}) is None
    monkeypatch.chdir(b)
    assert toolchain.which("tool", {"PATH": f"{a}:"}) == "tool"           # an empty entry is the working folder
    # no PATH at all: os.defpath, where subprocess looks too (os.get_exec_path)
    expected = next((os.path.join(d, "true") for d in os.defpath.split(os.pathsep)
                     if os.access(os.path.join(d, "true"), os.X_OK)), None)
    assert toolchain.which("true", {}) == expected


def test_is_shim_follows_links_and_ignores_case(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    shim_dir = tmp_path / "usr-bin"
    shim = program(shim_dir / "git")
    monkeypatch.setattr(toolchain, "SHIM_DIR", str(shim_dir))
    link = tmp_path / "links" / "git"
    link.parent.mkdir()
    link.symlink_to(shim)
    assert toolchain.is_shim(str(shim)) and toolchain.is_shim(str(link))
    assert not toolchain.is_shim(str(program(tmp_path / "homebrew" / "git")))
    upper = Path(str(shim_dir).upper()) / "git"
    if upper.exists():                                            # a case-insensitive volume (APFS's default)
        assert toolchain.is_shim(str(upper))


# ---- git_usable ------------------------------------------------------------------------------------------------

def test_git_usable_is_never_true_in_eve_lite(no_tools: Path, tmp_path: Path) -> None:
    brew = program(tmp_path / "brew" / "bin" / "git").parent
    git_in(Path(toolchain.CLT_DIR))
    assert toolchain.git_usable({"PATH": str(brew)}) is True                       # control: full eve runs it
    assert toolchain.git_usable({"PATH": str(brew), "EVE_PROFILE": "lite"}) is False
    assert toolchain.git_usable({"PATH": str(brew), "EVE_PROFILE": "full"}) is True


def test_a_git_outside_usr_bin_is_git_without_the_tools(no_tools: Path, tmp_path: Path) -> None:
    brew = program(tmp_path / "brew" / "bin" / "git").parent
    assert toolchain.dev_tools_present({}) is False
    assert toolchain.git_usable({"PATH": str(brew)}) is True


def test_usr_bin_git_counts_only_with_the_tools(no_tools: Path, tmp_path: Path,
                                                monkeypatch: pytest.MonkeyPatch) -> None:
    shim_dir = tmp_path / "usr-bin"
    shim = program(shim_dir / "git")
    monkeypatch.setattr(toolchain, "SHIM_DIR", str(shim_dir))
    links = tmp_path / "links"
    links.mkdir()
    (links / "git").symlink_to(shim)
    for path in (str(shim_dir), str(links)):
        assert toolchain.git_usable({"PATH": path}) is False      # the shim, no tools: running it is the dialog
    git_in(Path(toolchain.CLT_DIR))
    for path in (str(shim_dir), str(links)):
        assert toolchain.git_usable({"PATH": path}) is True


def test_no_git_at_all_is_not_usable(no_tools: Path, tmp_path: Path) -> None:
    git_in(Path(toolchain.CLT_DIR))
    (tmp_path / "empty").mkdir()
    assert toolchain.git_usable({"PATH": str(tmp_path / "empty")}) is False


def test_off_macos_git_runs_as_it_always_has(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """Linux keeps real git in /usr/bin and has none of the folders xcode-select looks in, and on Windows the file is
    git.exe: neither has a shim, so git_usable is True there and full eve keeps its records' history (bridge/README
    lists macOS, Linux and Windows). Control: the same folder on a Mac without the tools is the shim."""
    usr_bin = tmp_path / "usr" / "bin"
    program(usr_bin / "git")
    monkeypatch.setattr(toolchain, "SHIM_DIR", str(usr_bin))
    monkeypatch.setattr(toolchain, "developer_dirs", lambda environ=None: [])
    monkeypatch.delenv("EVE_PROFILE", raising=False)
    env = {"PATH": str(usr_bin)}
    assert toolchain.is_shim(str(usr_bin / "git")) and toolchain.dev_tools_present(env) is False
    for platform in ("linux", "win32"):
        monkeypatch.setattr(toolchain.sys, "platform", platform)
        assert toolchain.git_usable(env) is True, platform
        assert toolchain.git_usable({}) is True, platform     # no git found: subprocess says so, as it always did
        assert toolchain.git_usable({**env, "EVE_PROFILE": "lite"}) is False, platform
    monkeypatch.setattr(toolchain.sys, "platform", "darwin")
    assert toolchain.git_usable(env) is False                    # control: a Mac without the tools


def test_off_macos_records_run_git_as_before(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    started: list[list[str]] = []

    def run(argv, *a, **k):
        started.append(list(argv))
        return subprocess.CompletedProcess(argv, 0, "", "")

    monkeypatch.setattr(records.subprocess, "run", run)
    monkeypatch.setattr(toolchain, "developer_dirs", lambda environ=None: [])
    monkeypatch.delenv("EVE_PROFILE", raising=False)
    monkeypatch.setattr(toolchain.sys, "platform", "linux")
    records._git(tmp_path, "status")
    assert started == [["git", "-C", str(tmp_path), "status"]]


def test_this_mac(monkeypatch: pytest.MonkeyPatch) -> None:
    """On a Mac that has the Command Line Tools (the owner's), they are found, and its git is usable in full eve:
    nothing changes for the owner."""
    if not os.access("/Library/Developer/CommandLineTools/usr/bin/git", os.X_OK):
        pytest.skip("no Command Line Tools on this machine")
    monkeypatch.delenv("EVE_PROFILE", raising=False)
    assert toolchain.dev_tools_present() is True
    if toolchain.which("git") is not None:
        assert toolchain.git_usable() is True


def test_the_shim_names_match_this_macs_usr_bin() -> None:
    """Every program in /usr/bin that is git's file (or xcodebuild's) under another name is in the lists, so the
    spawn scan (test_no_devtool_spawns.py) looks for all of them. One way only: another macOS may have fewer."""
    assert len(toolchain.SHIMS) == 78 and "xcrun" in toolchain.XCODE_SHIMS
    if sys.platform != "darwin" or not os.path.exists("/usr/bin/git"):
        pytest.skip("not a Mac with /usr/bin/git")
    for anchor, names in (("git", toolchain.SHIMS), ("xcodebuild", toolchain.XCODE_SHIMS)):
        if not os.path.exists(f"/usr/bin/{anchor}"):
            continue
        st = os.stat(f"/usr/bin/{anchor}")
        same = {e.name for e in os.scandir("/usr/bin")
                if e.is_file() and e.stat().st_ino == st.st_ino and e.stat().st_dev == st.st_dev}
        assert anchor in same                                     # positive control: the scan finds the anchor
        assert same <= names, sorted(same - names)


# ---- records._git: asks before every git ---------------------------------------------------------------------------

@pytest.fixture
def unusable(monkeypatch: pytest.MonkeyPatch) -> list[list[str]]:
    """A Mac without the developer tools: `which git` finds /usr/bin/git (the shim), git_usable says no, and any
    process records.py would start is recorded instead of run."""
    started: list[list[str]] = []
    monkeypatch.setattr(records.shutil, "which", lambda name, *a, **k: "/usr/bin/git" if name == "git" else None)
    monkeypatch.setattr(toolchain, "git_usable", lambda environ=None: False)

    def spy(argv, *a, **k):
        started.append(list(argv))
        raise AssertionError("records started a process")

    monkeypatch.setattr(records.subprocess, "run", spy)
    return started


def test_records_git_raises_oserror_before_any_process(tmp_path: Path, unusable: list[list[str]]) -> None:
    with pytest.raises(OSError):
        records._git(tmp_path, "status")
    assert unusable == []


def test_records_degrade_with_one_log_line_each(tmp_path: Path, unusable: list[list[str]],
                                                caplog: pytest.LogCaptureFixture) -> None:
    repo = tmp_path / "records"
    caplog.set_level(logging.WARNING, logger="eve_bridge.records")
    assert records.ensure_repo(repo) is False
    assert records.commit(repo, "a dream") is None
    assert records.squash_history(repo) is False
    assert unusable == [] and not (repo / ".git").exists()
    lines = [r.getMessage() for r in caplog.records]
    assert len(lines) == 3 and all("OSError" in line for line in lines), lines


def test_in_eve_lite_records_start_no_git(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    started: list[list[str]] = []
    monkeypatch.setattr(records.subprocess, "run", lambda argv, *a, **k: started.append(list(argv)))
    monkeypatch.setenv("EVE_PROFILE", "lite")
    with pytest.raises(OSError):
        records._git(tmp_path, "status")
    assert records.commit(tmp_path, "a dream") is None
    assert started == []


def test_where_git_is_usable_records_keep_their_history(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """Positive control for the guard: on this Mac, with git usable, the records repository and a commit happen."""
    monkeypatch.delenv("EVE_PROFILE", raising=False)
    if not toolchain.git_usable():
        pytest.skip("git is not usable on this machine")
    repo = tmp_path / "records"
    assert records.ensure_repo(repo) is True
    (repo / "dining.md").write_text("- Loves pasta\n")
    assert records.commit(repo, "a dream")
    assert records._git(repo, "log", "--format=%s").stdout.splitlines() == ["a dream"]
