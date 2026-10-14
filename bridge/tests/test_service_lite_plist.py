"""eve lite's LaunchAgent (_service_launchd's lite unit, docs/plans/eve-lite.md M8 and WP-10), beside full eve's.

* The lite plist holds exactly the spec's keys: its own label, the venv's python running `-m eve_bridge.cli daemon`,
  a restart only after a failure exit and 5 s later, Interactive, umask 077, eve-lite.log, and an environment of HOME,
  launchd's PATH, EVE_PROFILE=lite and PYTHONNOUSERSITE. Its paths follow HOME when it is written, and nothing of full
  eve's (the serial port, Claude homes, Homebrew) reaches it.
* Full eve's plists are byte for byte what main recorded in the owner golden (tools/owner_golden.py), with a control.
* install_lite writes the plist (0644, whole) and loads it; it refuses beside full eve's agent, and a failed load is
  an exit 2. uninstall_lite stops eve lite by its label and removes the file; lite_loaded matches the label whole.

launchctl never runs here: every call goes to a recorder, and HOME is a temporary folder, so nothing reaches the
owner's ~/Library or launchd.
"""

from __future__ import annotations

import os
import plistlib
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Any

import owner_golden
import pytest

from eve_bridge import _service_launchd, profile

LITE_KEYS = {"Label", "ProgramArguments", "RunAtLoad", "KeepAlive", "ThrottleInterval", "ProcessType", "Umask",
             "StandardOutPath", "StandardErrorPath", "EnvironmentVariables"}


class Launchctl:
    """subprocess.run for launchctl: records each call and answers as launchd would; ``listed`` is what `list` shows."""

    def __init__(self, *, listed: tuple[str, ...] = (), fail: tuple[str, ...] = ()) -> None:
        self.calls: list[list[str]] = []
        self.listed = listed
        self.fail = fail

    def __call__(self, argv: Any, *args: Any, **kwargs: Any) -> subprocess.CompletedProcess:
        argv = [str(a) for a in argv]
        assert argv[0] == "launchctl", f"only launchctl runs here, not {argv[0]}"
        self.calls.append(argv)
        verb = argv[1]
        if verb == "list":
            out = "PID\tStatus\tLabel\n" + "".join(f"-\t0\t{label}\n" for label in self.listed)
            return subprocess.CompletedProcess(argv, 0, out, "")
        code = 5 if verb in self.fail else 0
        return subprocess.CompletedProcess(argv, code, "", "Load failed: 5: Input/output error" if code else "")

    def verbs(self) -> list[str]:
        return [call[1] for call in self.calls]


@pytest.fixture(autouse=True)
def _no_launchd(monkeypatch: pytest.MonkeyPatch) -> None:
    """No test here reaches launchd: launchctl is not found unless a test puts its recorder in."""
    which = shutil.which
    monkeypatch.setattr(shutil, "which", lambda name, *a, **k: None if name == "launchctl" else which(name, *a, **k))


@pytest.fixture
def home(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """A fresh HOME: Path.home() and every lite path follow it."""
    folder = tmp_path / "home"
    folder.mkdir()
    monkeypatch.setenv("HOME", str(folder))
    assert Path.home() == folder
    return folder


@pytest.fixture
def launchctl(monkeypatch: pytest.MonkeyPatch) -> Launchctl:
    fake = Launchctl()
    monkeypatch.setattr(shutil, "which", lambda name, *a, **k: "/bin/launchctl" if name == "launchctl" else None)
    monkeypatch.setattr(subprocess, "run", fake)
    return fake


def _lite() -> dict[str, Any]:
    return plistlib.loads(_service_launchd._build_lite_plist())


# ---- the plist ------------------------------------------------------------------------------------------------------

def test_the_lite_plist_holds_exactly_the_specs_keys_and_values(home: Path) -> None:
    unit = _lite()
    log = str(home / "Library" / "Logs" / "eve-lite.log")
    assert set(unit) == LITE_KEYS
    assert unit["Label"] == "com.github.eve-bridge.lite" == _service_launchd.LITE_LABEL
    assert unit["ProgramArguments"] == [sys.executable, "-m", "eve_bridge.cli", "daemon"]
    assert unit["RunAtLoad"] is True
    assert unit["KeepAlive"] == {"SuccessfulExit": False}       # 75 (a sign-in) comes back; 0 (a stop) stays down
    assert unit["ThrottleInterval"] == 5
    assert unit["ProcessType"] == "Interactive"
    assert unit["Umask"] == 63 == 0o077
    assert unit["StandardOutPath"] == unit["StandardErrorPath"] == log
    assert unit["EnvironmentVariables"] == {"HOME": str(home), "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
                                            "EVE_PROFILE": "lite", "PYTHONNOUSERSITE": "1"}


def test_the_pinned_profile_is_the_one_profile_py_reads_as_lite(home: Path) -> None:
    """The plist pins the profile because the env file only fills gaps (envfile.py): a daemon started by it is eve lite
    whatever the env file says."""
    env = _lite()["EnvironmentVariables"]
    assert profile.is_lite(env) and profile.active(env) == profile.LITE
    assert not profile.is_lite({k: v for k, v in env.items() if k != profile.PROFILE_ENV})      # control


def test_the_lite_plist_follows_home_when_it_is_written(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    for name in ("one", "two"):
        folder = tmp_path / name
        folder.mkdir()
        monkeypatch.setenv("HOME", str(folder))
        unit = _lite()
        assert unit["EnvironmentVariables"]["HOME"] == str(folder)
        assert unit["StandardOutPath"].startswith(str(folder) + "/")
        assert _service_launchd.lite_plist_path() == folder / "Library" / "LaunchAgents" / "com.github.eve-bridge.lite.plist"


def test_nothing_of_full_eves_unit_reaches_the_lite_plist(home: Path, tmp_path: Path,
                                                          monkeypatch: pytest.MonkeyPatch) -> None:
    before = _service_launchd._build_lite_plist()
    monkeypatch.setenv("CLAUDE_CONFIG_DIR", str(tmp_path / "wrapper-claude"))
    monkeypatch.setenv("EVE_SERIAL_PORT", "/dev/cu.usbmodem*")
    after = _service_launchd._build_lite_plist()
    assert after == before
    text = after.decode()
    for word in ("com.github.eve-bridge.daemon", "EVE_SERIAL_PORT", "EVE_CLAUDE_CONFIG_DIRS", "/opt/homebrew",
                 "eve-bridge.log"):
        assert word not in text, word
    # control: full eve's own plist, built in the same environment, carries both
    full = _service_launchd._build_plist("/dev/cu.usbmodem*").decode()
    assert "EVE_SERIAL_PORT" in full and "EVE_CLAUDE_CONFIG_DIRS" in full


@pytest.mark.skipif(not Path("/usr/bin/plutil").exists(), reason="plutil is macOS's")
def test_plutil_reads_the_lite_plist(tmp_path: Path, home: Path) -> None:
    path = tmp_path / "lite.plist"
    path.write_bytes(_service_launchd._build_lite_plist())
    done = subprocess.run(["/usr/bin/plutil", "-lint", str(path)], capture_output=True, text=True, check=False)
    assert done.returncode == 0 and done.stdout.strip().endswith("OK"), done.stdout + done.stderr


# ---- full eve's plists: the owner golden --------------------------------------------------------------------------

def test_full_eves_plists_are_byte_for_byte_the_golden() -> None:
    golden = owner_golden.load()["snapshot"]["plist"]
    now = owner_golden.plists()
    assert set(now) == set(golden) == {"daemon_serial", "daemon_plain", "daemon_wrapper_home", "widget"}
    for name, unit in golden.items():
        assert now[name]["xml"] == unit["xml"], name
        assert now[name]["sha256"] == unit["sha256"], name


def test_a_full_plist_changed_in_code_does_not_match_the_golden(monkeypatch: pytest.MonkeyPatch) -> None:
    """Positive control for the test above: a changed label shows in every daemon unit, not in the widget's."""
    golden = owner_golden.load()["snapshot"]["plist"]
    monkeypatch.setattr(_service_launchd, "LABEL", "com.example.another-daemon")
    now = owner_golden.plists()
    assert {n for n in golden if now[n]["xml"] != golden[n]["xml"]} == {"daemon_serial", "daemon_plain",
                                                                         "daemon_wrapper_home"}


# ---- install_lite and uninstall_lite --------------------------------------------------------------------------------

def test_install_lite_writes_the_plist_and_loads_it(home: Path, launchctl: Launchctl,
                                                    capsys: pytest.CaptureFixture[str]) -> None:
    assert _service_launchd.install_lite() == 0
    path = home / "Library" / "LaunchAgents" / "com.github.eve-bridge.lite.plist"
    assert path.read_bytes() == _service_launchd._build_lite_plist()
    assert path.stat().st_mode & 0o777 == 0o644
    assert (home / "Library" / "Logs").is_dir()
    assert launchctl.calls == [["launchctl", "unload", str(path)], ["launchctl", "load", "-w", str(path)]]
    assert [p.name for p in path.parent.iterdir()] == [path.name]            # no temporary file left beside it
    out = capsys.readouterr().out
    assert f"installed: {path}" in out and str(home / "Library" / "Logs" / "eve-lite.log") in out


def test_install_lite_again_replaces_the_plist_with_this_interpreter(home: Path, launchctl: Launchctl,
                                                                     monkeypatch: pytest.MonkeyPatch) -> None:
    """An update runs `install --service` again: the plist then names the new venv's python, and is reloaded."""
    assert _service_launchd.install_lite() == 0
    monkeypatch.setattr(sys, "executable", str(home / ".eve-lite" / "venv" / "bin" / "python"))
    assert _service_launchd.install_lite() == 0
    assert _lite()["ProgramArguments"][0] == str(home / ".eve-lite" / "venv" / "bin" / "python")
    unit = plistlib.loads(_service_launchd.lite_plist_path().read_bytes())
    assert unit["ProgramArguments"][0] == str(home / ".eve-lite" / "venv" / "bin" / "python")
    assert launchctl.verbs() == ["unload", "load", "unload", "load"]


def test_install_lite_refuses_beside_full_eves_agent(home: Path, launchctl: Launchctl,
                                                     capsys: pytest.CaptureFixture[str]) -> None:
    full = home / "Library" / "LaunchAgents" / "com.github.eve-bridge.daemon.plist"
    full.parent.mkdir(parents=True)
    full.write_bytes(b"full eve's")
    assert _service_launchd.install_lite() == 2
    assert launchctl.calls == []
    assert not _service_launchd.lite_plist_path().exists()
    assert full.read_bytes() == b"full eve's"
    assert "full eve's login service is installed" in capsys.readouterr().err


def test_install_lite_reads_full_eve_from_this_home_only(home: Path, launchctl: Launchctl) -> None:
    """Control for the refusal: the agent looked for is under HOME now, never a path fixed at import (on the owner's
    Mac, the module's own PLIST_PATH is full eve's real plist)."""
    assert _service_launchd.agent_path(_service_launchd.LABEL).parent == home / "Library" / "LaunchAgents"
    assert _service_launchd.install_lite() == 0


def test_a_failed_load_is_an_exit_2(home: Path, monkeypatch: pytest.MonkeyPatch,
                                    capsys: pytest.CaptureFixture[str]) -> None:
    fake = Launchctl(fail=("load",))
    monkeypatch.setattr(shutil, "which", lambda name, *a, **k: "/bin/launchctl")
    monkeypatch.setattr(subprocess, "run", fake)
    assert _service_launchd.install_lite() == 2
    assert "launchctl load failed (5)" in capsys.readouterr().err


def test_install_lite_says_so_when_launchagents_cannot_be_written(home: Path, launchctl: Launchctl,
                                                                  capsys: pytest.CaptureFixture[str]) -> None:
    (home / "Library").mkdir()
    (home / "Library" / "LaunchAgents").write_text("a file where the folder goes")
    assert _service_launchd.install_lite() == 2
    assert launchctl.calls == []
    assert "eve lite's login service could not be written" in capsys.readouterr().err


def test_install_lite_needs_launchctl(home: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(shutil, "which", lambda name, *a, **k: None)
    monkeypatch.setattr(subprocess, "run", Launchctl())
    assert _service_launchd.install_lite() == 2
    assert not _service_launchd.lite_plist_path().exists()


def test_uninstall_lite_stops_eve_lite_by_its_label_and_removes_the_plist(home: Path,
                                                                          monkeypatch: pytest.MonkeyPatch,
                                                                          capsys: pytest.CaptureFixture[str]) -> None:
    fake = Launchctl(listed=("com.github.eve-bridge.lite",))
    monkeypatch.setattr(shutil, "which", lambda name, *a, **k: "/bin/launchctl")
    monkeypatch.setattr(subprocess, "run", fake)
    path = _service_launchd.lite_plist_path()
    path.parent.mkdir(parents=True)
    path.write_bytes(_service_launchd._build_lite_plist())
    assert _service_launchd.uninstall_lite() == 0
    assert not path.exists()
    assert fake.calls[1:] == [["launchctl", "bootout", f"gui/{os.getuid()}/com.github.eve-bridge.lite"]]
    assert not any("com.github.eve-bridge.daemon" in " ".join(call) for call in fake.calls)
    assert capsys.readouterr().out == f"stopped eve lite\nremoved: {path}\n"


def test_uninstall_lite_unloads_by_file_when_bootout_fails(home: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    fake = Launchctl(listed=("com.github.eve-bridge.lite",), fail=("bootout",))
    monkeypatch.setattr(shutil, "which", lambda name, *a, **k: "/bin/launchctl")
    monkeypatch.setattr(subprocess, "run", fake)
    path = _service_launchd.lite_plist_path()
    path.parent.mkdir(parents=True)
    path.write_bytes(_service_launchd._build_lite_plist())
    assert _service_launchd.uninstall_lite() == 0
    assert fake.verbs() == ["list", "bootout", "unload"] and not path.exists()


def test_uninstall_lite_stops_a_loaded_agent_whose_file_is_gone(home: Path, monkeypatch: pytest.MonkeyPatch,
                                                                capsys: pytest.CaptureFixture[str]) -> None:
    fake = Launchctl(listed=("com.github.eve-bridge.lite",))
    monkeypatch.setattr(shutil, "which", lambda name, *a, **k: "/bin/launchctl")
    monkeypatch.setattr(subprocess, "run", fake)
    assert _service_launchd.uninstall_lite() == 0
    assert fake.verbs() == ["list", "bootout"]
    assert capsys.readouterr().out == "stopped eve lite\n"


def test_uninstall_lite_with_nothing_installed_does_nothing(home: Path, launchctl: Launchctl,
                                                            capsys: pytest.CaptureFixture[str]) -> None:
    assert _service_launchd.uninstall_lite() == 0
    assert launchctl.verbs() == ["list"]
    assert "not installed; nothing to do" in capsys.readouterr().out


@pytest.mark.parametrize("listed, loaded", [
    (("com.github.eve-bridge.lite",), True),
    (("com.github.eve-bridge.daemon",), False),                       # full eve's agent is not eve lite's
    (("com.github.eve-bridge.lite.helper", "x.com.github.eve-bridge.lite"), False),
    ((), False),
])
def test_lite_loaded_matches_the_label_whole(listed: tuple[str, ...], loaded: bool,
                                             monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(shutil, "which", lambda name, *a, **k: "/bin/launchctl")
    monkeypatch.setattr(subprocess, "run", Launchctl(listed=listed))
    assert _service_launchd.lite_loaded() is loaded


def test_installed_lite_plist_reads_only_a_real_plist(home: Path) -> None:
    assert _service_launchd.installed_lite_plist() is None
    path = _service_launchd.lite_plist_path()
    path.parent.mkdir(parents=True)
    path.write_bytes(b"not a plist")
    assert _service_launchd.installed_lite_plist() is None
    path.write_bytes(_service_launchd._build_lite_plist())
    assert _service_launchd.installed_lite_plist() == _lite()
    assert _service_launchd.lite_installed()
